use super::presentation::{
    DirectAction, DirectInputReservation, NativePresentationBarrier, PendingControlReview,
    PendingMetadataReview, ReserveDirectInputError,
};
use super::*;
use crate::session_controller::SessionController;
use serde_json::{json, Value};

fn ready_with(mut machine: Machine) -> Machine {
    assert!(matches!(
        machine.process_value(json!({
            "jsonrpc": "2.0", "id": 0, "method": "initialize",
            "params": {
                "protocolVersion": festerm_automation_mcp::PROTOCOL_VERSION,
                "clientInfo": {"name": "not an identity", "version": "0"},
                "capabilities": {},
            },
        })),
        Event::Reply(_)
    ));
    assert!(matches!(
        machine.process_value(json!({
            "jsonrpc": "2.0", "method": "notifications/initialized",
        })),
        Event::NoReply
    ));
    machine
}

fn ready() -> Machine {
    ready_with(Machine::new())
}

fn fixture() -> (Coordinator, FesTermApp, TabId, impl Fn() -> Option<Vec<u8>>) {
    let (app, id) = FesTermApp::automation_fixture();
    let mut coordinator = Coordinator::default();
    coordinator.broker = Some(Broker::new([1; 16]));
    coordinator.stop = Arc::new(AtomicBool::new(false));
    coordinator.privacy_gate = Arc::new(PrivacyGate::new(false));
    assert!(coordinator.privacy_gate.bind_mcp(&coordinator.stop));
    coordinator.enabled = true;
    let mut broker = coordinator.broker.take().unwrap();
    coordinator.reconcile(&mut broker, &[&app]);
    let principal = broker.accept_connection().unwrap();
    let (connection, receive) = ipc::test_connection(1);
    coordinator.peers.insert(
        1,
        Peer {
            connection,
            principal,
            machine: ready(),
            code: VerificationCode::new("0123456789abcdef").unwrap(),
            grant: None,
            waits: Vec::new(),
            controller: controller::PeerState::default(),
        },
    );
    coordinator.broker = Some(broker);
    (coordinator, app, id, receive)
}

struct SimulatedPresentationBarrier {
    monitor: festerm_automation::PresentationMonitorId,
    window: festerm_automation::PresentationWindowId,
    target: TargetRef,
    unavailable: bool,
    fail_after_operation: bool,
    change_before_operation: Option<WorkspaceRevision>,
}

impl super::presentation::NativePresentationBarrier for SimulatedPresentationBarrier {
    fn with_observation<T>(
        &mut self,
        broker: &mut Broker,
        workspace_revision: u64,
        operation: impl FnOnce(&mut Broker, festerm_automation::NativePresentationObservation) -> T,
    ) -> festerm_automation::Result<T> {
        use festerm_automation::{Error, NativePresentationState};
        use festerm_session::AutomationAdmissionTryError;

        assert!(matches!(
            festerm_session::try_automation_admission_lock(),
            Err(AutomationAdmissionTryError::Busy)
        ));
        if self.unavailable {
            return Err(Error::Unavailable);
        }
        let observation = broker
            .observe_presentation(
                self.monitor,
                NativePresentationState::Foreground {
                    window: self.window,
                    selected: Some(self.target),
                    workspace_revision,
                },
            )?
            .ok_or(Error::Unavailable)?;
        if let Some(workspace) = self.change_before_operation.take() {
            workspace.changed();
        }
        let result = operation(broker, observation);
        assert!(matches!(
            festerm_session::try_automation_admission_lock(),
            Err(AutomationAdmissionTryError::Busy)
        ));
        if self.fail_after_operation {
            Err(Error::Unavailable)
        } else {
            Ok(result)
        }
    }
}

struct PresentedInputFixture {
    coordinator: Coordinator,
    app: FesTermApp,
    tab: TabId,
    ime: Arc<festerm_session::AutomationEpoch>,
    workspace: WorkspaceRevision,
    run: RunId,
    lease: festerm_automation::LeaseRef,
    requirement: festerm_automation::PresentationRequirement,
    request: RequestId,
    barrier: SimulatedPresentationBarrier,
}

fn presentation_admission_test_guard() -> std::sync::MutexGuard<'static, ()> {
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner())
}

fn reserve_presented_input(
    coordinator: &Coordinator,
    target: TargetRef,
    bytes: &[u8],
) -> (
    festerm_session::ReservedAutomationInput,
    festerm_session::PendingAutomationInput,
) {
    let target = &coordinator.targets[&target.session()];
    reserve_for_binding(target.controller_instance, target.attachment.clone(), bytes)
}

fn reserve_for_binding(
    controller_instance: u64,
    attachment: Arc<festerm_session::AutomationEpoch>,
    bytes: &[u8],
) -> (
    festerm_session::ReservedAutomationInput,
    festerm_session::PendingAutomationInput,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match festerm_session::automation_input_slot(bytes, controller_instance, attachment.clone())
        {
            Err(festerm_session::ReserveAutomationInputError::Busy)
                if Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            result => return result.unwrap(),
        }
    }
}

fn reserve_direct_input_with_retry(
    coordinator: &mut Coordinator,
    channel: u64,
    request: RequestId,
    tab: &SessionTab,
) -> Result<DirectInputReservation, ReserveDirectInputError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let result = coordinator.reserve_direct_input(channel, request, tab);
        if matches!(
            &result,
            Err(ReserveDirectInputError::Authority(
                festerm_automation::Error::Busy
            )) | Err(ReserveDirectInputError::Backend(
                festerm_session::ReserveAutomationInputError::Busy
            ))
        ) {
            assert!(
                Instant::now() < deadline,
                "automation reservation stayed busy"
            );
            std::thread::sleep(Duration::from_millis(1));
        } else {
            return result;
        }
    }
}

fn retry_busy_case(mut attempt: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !attempt() {
        assert!(
            Instant::now() < deadline,
            "automation admission stayed busy"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn presentation_control_request(target: TargetRef) -> (GrantRequest, Scope, InvocationBinding) {
    let mut scope = Scope::default();
    scope.control.insert(target);
    scope.invoke_workflows = true;
    scope.return_results = true;
    let binding = InvocationBinding {
        api_version: 1,
        invocation: Invocation::Workflow {
            bundle: Digest([1; 32]),
            manifest: Digest([2; 32]),
        },
        parameters: Digest([3; 32]),
    };
    let request = GrantRequest {
        scope: scope.clone(),
        bindings: BTreeSet::from([binding]),
        direct_read_calls: BTreeSet::new(),
    };
    (request, scope, binding)
}

fn prepare_control_review<S: festerm_session::Session>(
    coordinator: &mut Coordinator,
    channel: u64,
    target: TargetRef,
    request: &GrantRequest,
    controller: &mut SessionController<S>,
) -> PendingControlReview {
    retry_automation_busy(|| {
        coordinator.prepare_control_handoff_review(
            channel,
            target,
            request.clone(),
            Duration::from_secs(60),
            controller,
        )
    })
    .unwrap()
}

fn approve_control_review(
    coordinator: &mut Coordinator,
    channel: u64,
    review: &PendingControlReview,
) -> festerm_automation::Result<()> {
    let decision = review.accept_from_native();
    retry_automation_busy(|| coordinator.approve_control_handoff(channel, review, &decision))
}

fn prepare_metadata_review(
    coordinator: &mut Coordinator,
    channel: u64,
    targets: &[TargetRef],
) -> PendingMetadataReview {
    retry_automation_busy(|| {
        coordinator.prepare_metadata_review(
            channel,
            festerm_automation::MetadataGrantRequest {
                targets: targets.iter().copied().collect(),
            },
            Duration::from_secs(60),
        )
    })
    .unwrap()
}

fn approve_metadata_review(
    coordinator: &mut Coordinator,
    channel: u64,
    review: &PendingMetadataReview,
) -> festerm_automation::Result<()> {
    let decision = review.accept_from_native();
    retry_automation_busy(|| coordinator.approve_metadata_review(channel, review, &decision))
}

fn metadata_disclosure(
    coordinator: &mut Coordinator,
    channel: u64,
    targets: &[TargetRef],
) -> (RunId, RequestId, festerm_automation::DisclosureLiveness) {
    metadata_disclosure_with_deadline(coordinator, channel, targets, Duration::from_secs(30))
}

fn metadata_disclosure_with_deadline(
    coordinator: &mut Coordinator,
    channel: u64,
    targets: &[TargetRef],
    duration: Duration,
) -> (RunId, RequestId, festerm_automation::DisclosureLiveness) {
    let principal = coordinator.peers[&channel].principal;
    let now = Instant::now();
    let broker = coordinator.broker.as_mut().unwrap();
    let run = broker
        .admit_metadata_run(
            principal,
            festerm_automation::MetadataRunSpec {
                parameters: Digest([19; 32]),
                targets: targets.iter().copied().collect(),
                budget: Budget::default(),
                deadline: now + duration,
            },
            now,
        )
        .unwrap();
    let request = broker
        .prepare_request(
            principal,
            run,
            AuthorityOperation::Discover { max_bytes: 64 },
            now,
        )
        .unwrap();
    let mut exact_targets = targets.to_vec();
    exact_targets.sort();
    assert_eq!(
        broker.discovery_targets(principal, request, now).unwrap(),
        exact_targets
    );
    broker
        .commit_disclosure(principal, request, 8, now)
        .unwrap();
    let liveness = broker.disclosure_liveness(principal, request, now).unwrap();
    (run, request, liveness)
}

fn add_metadata_peer(coordinator: &mut Coordinator, channel: u64) -> ConnectionId {
    let principal = coordinator
        .broker
        .as_mut()
        .unwrap()
        .accept_connection()
        .unwrap();
    let (connection, _receive) = ipc::test_connection(channel);
    coordinator.peers.insert(
        channel,
        Peer {
            connection,
            principal,
            machine: ready(),
            code: VerificationCode::new("0123456789abcdef").unwrap(),
            grant: None,
            waits: Vec::new(),
            controller: controller::PeerState::default(),
        },
    );
    principal
}

fn commit_presented_input_with_retry<B: NativePresentationBarrier>(
    coordinator: &mut Coordinator,
    channel: u64,
    request: RequestId,
    barrier: &mut B,
    mut reserve: impl FnMut(
        &Coordinator,
    ) -> (
        festerm_session::ReservedAutomationInput,
        festerm_session::PendingAutomationInput,
    ),
) -> (
    std::result::Result<(), festerm_session::CommitAutomationInputError<festerm_automation::Error>>,
    festerm_session::PendingAutomationInput,
) {
    use festerm_automation::Error;
    use festerm_session::{AutomationInputStatus, CommitAutomationInputError};

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (reservation, mut pending) = reserve(coordinator);
        let result = coordinator.commit_presented_input(channel, request, reservation, barrier);
        if matches!(
            result,
            Err(CommitAutomationInputError::Refused(Error::Busy))
        ) && Instant::now() < deadline
        {
            assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
            std::thread::sleep(Duration::from_millis(1));
        } else {
            return (result, pending);
        }
    }
}

fn commit_presented_input_retry(
    coordinator: &mut Coordinator,
    channel: u64,
    request: RequestId,
    barrier: &mut SimulatedPresentationBarrier,
    bytes: &[u8],
) -> (
    std::result::Result<(), festerm_session::CommitAutomationInputError<festerm_automation::Error>>,
    festerm_session::PendingAutomationInput,
) {
    let target = barrier.target;
    commit_presented_input_with_retry(coordinator, channel, request, barrier, |coordinator| {
        reserve_presented_input(coordinator, target, bytes)
    })
}

fn commit_direct_input_with_retry<B: NativePresentationBarrier>(
    coordinator: &mut Coordinator,
    channel: u64,
    request: RequestId,
    mut barrier: Option<&mut B>,
    mut reserve: impl FnMut(
        &Coordinator,
    ) -> (
        festerm_session::ReservedAutomationInput,
        festerm_session::PendingAutomationInput,
    ),
    tab: &SessionTab,
) -> (
    std::result::Result<(), festerm_session::CommitAutomationInputError<festerm_automation::Error>>,
    festerm_session::PendingAutomationInput,
) {
    use festerm_automation::Error;
    use festerm_session::{AutomationInputStatus, CommitAutomationInputError};
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (reservation, mut pending) = reserve(coordinator);
        let result = coordinator.commit_direct_input(
            channel,
            request,
            DirectInputReservation::for_test(reservation, channel, request),
            tab,
            barrier.as_deref_mut(),
        );
        if matches!(
            result,
            Err(CommitAutomationInputError::Refused(Error::Busy))
        ) && Instant::now() < deadline
        {
            assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
            std::thread::sleep(Duration::from_millis(1));
        } else {
            return (result, pending);
        }
    }
}

fn commit_direct_input_retry(
    coordinator: &mut Coordinator,
    channel: u64,
    request: RequestId,
    barrier: &mut SimulatedPresentationBarrier,
    bytes: &[u8],
    tab: &SessionTab,
) -> (
    std::result::Result<(), festerm_session::CommitAutomationInputError<festerm_automation::Error>>,
    festerm_session::PendingAutomationInput,
) {
    let target = barrier.target;
    commit_direct_input_with_retry(
        coordinator,
        channel,
        request,
        Some(barrier),
        |coordinator| reserve_presented_input(coordinator, target, bytes),
        tab,
    )
}

fn control_review_fixture() -> (
    Coordinator,
    FesTermApp,
    TabId,
    Arc<festerm_session::AutomationEpoch>,
    TargetRef,
) {
    control_review_fixture_with_sources(&[])
}

fn control_review_fixture_with_sources(
    sources: &[&FesTermApp],
) -> (
    Coordinator,
    FesTermApp,
    TabId,
    Arc<festerm_session::AutomationEpoch>,
    TargetRef,
) {
    let (mut coordinator, mut app, tab, _receive) = fixture();
    if !sources.is_empty() {
        let apps: Vec<_> = std::iter::once(&app)
            .chain(sources.iter().copied())
            .collect();
        let mut broker = coordinator.broker.take().unwrap();
        coordinator.reconcile(&mut broker, &apps);
        coordinator.broker = Some(broker);
    }
    let ime = Arc::new(festerm_session::AutomationEpoch::new());
    ime.transition(true);
    app.set_automation_ime_epoch(&ime);
    let target = coordinator.targets.get_mut(&tab.chip_id()).unwrap();
    // This fake transport is used only to exercise the local-only admission seam.
    target.local_pty = true;
    let target = target.reference;
    (coordinator, app, tab, ime, target)
}

fn presented_input_fixture() -> PresentedInputFixture {
    presented_input_fixture_with_sources(&[])
}

fn presented_input_fixture_with_sources(
    sources: &[(&FesTermApp, TabId, bool)],
) -> PresentedInputFixture {
    presented_input_fixture_with_binding(sources, false)
}

fn presented_input_fixture_with_binding(
    sources: &[(&FesTermApp, TabId, bool)],
    direct_input: bool,
) -> PresentedInputFixture {
    presented_input_fixture_with_options(sources, direct_input, false)
}

fn presented_input_fixture_with_options(
    sources: &[(&FesTermApp, TabId, bool)],
    direct_input: bool,
    include_target_source: bool,
) -> PresentedInputFixture {
    use festerm_automation::{
        NativeControllerEnrollment, NativeControllerPresentationApproval, NativePresentationState,
        PresentationRequirement, PresentationWindowId,
    };

    let source_apps: Vec<_> = sources.iter().map(|(app, _, _)| *app).collect();
    let (mut coordinator, mut app, tab, ime, target) =
        control_review_fixture_with_sources(&source_apps);
    let principal = coordinator.peers[&1].principal;
    let now = Instant::now();
    let (mut request, mut scope, mut binding) = presentation_control_request(target);
    for (app, tab, discover) in sources {
        assert!(app.automation_session(*tab).is_some());
        let source = coordinator.targets[&tab.chip_id()].reference;
        if *discover {
            request.scope.discover.insert(source);
        } else {
            request.scope.observe.insert(source);
        }
    }
    if include_target_source {
        request.scope.discover.insert(target);
    }
    let review = if direct_input {
        let review = retry_automation_busy(|| {
            coordinator.prepare_direct_action_review_with_sources(
                1,
                target,
                DirectAction::Text {
                    text: "test",
                    submit: false,
                },
                Duration::from_secs(60),
                app.automation_session_mut(tab),
                &request.scope,
            )
        })
        .unwrap();
        request = review.request().clone();
        scope = request.scope.clone();
        binding = *request.bindings.first().unwrap();
        review
    } else {
        prepare_control_review(
            &mut coordinator,
            1,
            target,
            &request,
            &mut app.automation_session_mut(tab).controller,
        )
    };
    approve_control_review(&mut coordinator, 1, &review).unwrap();
    let run = coordinator
        .broker
        .as_mut()
        .unwrap()
        .admit_run(
            principal,
            None,
            RunSpec {
                binding,
                scope,
                budget: Budget::default(),
                deadline: now + Duration::from_secs(30),
            },
            now,
        )
        .unwrap();
    let lease =
        retry_automation_busy(|| coordinator.acquire_control_lease(1, run, target)).unwrap();
    let broker = coordinator.broker.as_mut().unwrap();
    let registration = broker
        .register_controller_session(principal, b"controller")
        .unwrap();
    let proposal = broker
        .prepare_controller_enrollment(principal, registration, target)
        .unwrap();
    let enrollment = broker
        .approve_controller_enrollment(principal, NativeControllerEnrollment::new(proposal))
        .unwrap();
    let proposal = broker
        .prepare_controller_presentation_approval(principal, registration, enrollment, now)
        .unwrap();
    broker
        .approve_controller_presentation(
            principal,
            NativeControllerPresentationApproval::new(proposal, Duration::from_secs(60)),
            now,
        )
        .unwrap();
    let monitor = broker.start_qualified_presentation_monitor().unwrap();
    let window = PresentationWindowId::from_host(1);
    let attempt = broker
        .prepare_presentation(
            principal,
            registration,
            enrollment,
            window,
            Duration::from_secs(2),
            now,
        )
        .unwrap();
    broker
        .presentation_requested(principal, attempt, now)
        .unwrap();
    let observation = broker
        .observe_presentation(
            monitor,
            NativePresentationState::Foreground {
                window,
                selected: Some(target),
                workspace_revision: 0,
            },
        )
        .unwrap()
        .unwrap();
    let confirmed = broker
        .finish_presentation(principal, attempt, Some(observation), now)
        .unwrap()
        .unwrap();
    let requirement = PresentationRequirement::new(registration, enrollment, confirmed.revision);
    if direct_input {
        for (input, bytes) in [
            (Digest(Sha256::digest(b"nope").into()), 4),
            (Digest(Sha256::digest(b"test").into()), 5),
        ] {
            assert_eq!(
                broker.prepare_request(
                    principal,
                    run,
                    AuthorityOperation::Control {
                        lease,
                        input,
                        bytes,
                        presentation: Some(requirement),
                    },
                    now,
                ),
                Err(festerm_automation::Error::BindingMismatch)
            );
        }
    }
    let request = broker
        .prepare_request(
            principal,
            run,
            AuthorityOperation::Control {
                lease,
                input: Digest(Sha256::digest(b"test").into()),
                bytes: 4,
                presentation: Some(requirement),
            },
            now,
        )
        .unwrap();
    let workspace = WorkspaceRevision::default();
    coordinator.bind_presentation_workspace(&workspace).unwrap();
    PresentedInputFixture {
        coordinator,
        app,
        tab,
        ime,
        workspace,
        run,
        lease,
        requirement,
        request,
        barrier: SimulatedPresentationBarrier {
            monitor,
            window,
            target,
            unavailable: false,
            fail_after_operation: false,
            change_before_operation: None,
        },
    }
}

fn disconnect_source_without_reconciliation(app: &FesTermApp, tab: TabId) {
    let controller = &app.automation_session(tab).unwrap().controller;
    let Some(crate::tabs::ApplicationSession::TestSsh(fake)) = controller.session() else {
        panic!("expected fake SSH attachment");
    };
    fake.push_event(festerm_session::SessionEvent::Lifecycle(
        festerm_session::SessionLifecycle::Disconnected(festerm_session::SessionError::new(
            festerm_session::SessionErrorKind::Input,
            "source disconnected",
        )),
    ));
    assert_eq!(
        controller.lifecycle(),
        Some(festerm_session::SessionLifecycle::Running)
    );
}

fn replace_source_attachment_without_event(app: &FesTermApp, tab: TabId) {
    let Some(crate::tabs::ApplicationSession::TestSsh(fake)) =
        app.automation_session(tab).unwrap().controller.session()
    else {
        panic!("expected fake SSH attachment");
    };
    fake.replace_attachment_without_event();
}

fn change_readable_source(
    app: &mut FesTermApp,
    tab: TabId,
    coordinator: &mut Coordinator,
    change: usize,
) {
    match change {
        0 => disconnect_source_without_reconciliation(app, tab),
        1 => replace_source_attachment_without_event(app, tab),
        2 => {
            let controller = &app.automation_session(tab).unwrap().controller;
            let instance = controller.automation_instance();
            let generation = controller.lifecycle_generation();
            assert_eq!(app.automation_replace_fixture_session(), tab);
            let controller = &app.automation_session(tab).unwrap().controller;
            assert_ne!(controller.automation_instance(), instance);
            assert_eq!(controller.lifecycle_generation(), generation);
        }
        3 => app
            .automation_session_mut(tab)
            .controller
            .advance_lifecycle_generation(),
        4 => set_admission_flag(&coordinator.targets[&tab.chip_id()].registration, false),
        5 => {
            let replacement = Arc::new(festerm_session::AutomationEpoch::new());
            replacement.transition(true);
            let target = coordinator.targets.get_mut(&tab.chip_id()).unwrap();
            assert_eq!(replacement.capture(), Some(target.attachment_generation));
            target.attachment = replacement;
        }
        _ => unreachable!(),
    }
}

#[test]
fn presentation_input_admission_rechecks_native_barrier_and_exact_reserved_bytes() {
    use festerm_session::AutomationInputStatus;

    let _serial = presentation_admission_test_guard();
    let PresentedInputFixture {
        mut coordinator,
        app: _app,
        request,
        mut barrier,
        ..
    } = presented_input_fixture();
    let (result, mut pending) =
        commit_presented_input_retry(&mut coordinator, 1, request, &mut barrier, b"test");
    result.unwrap();
    match pending.poll() {
        AutomationInputStatus::Ready(bytes) => assert_eq!(&*bytes, b"test"),
        _ => panic!("a successful admission must publish the exact reserved bytes"),
    }

    let PresentedInputFixture {
        mut coordinator,
        app: _app,
        request,
        mut barrier,
        ..
    } = presented_input_fixture();
    let (result, mut pending) =
        commit_presented_input_retry(&mut coordinator, 1, request, &mut barrier, b"wrong");
    assert!(matches!(
        result,
        Err(festerm_session::CommitAutomationInputError::Refused(
            festerm_automation::Error::BindingMismatch
        ))
    ));
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
}

#[test]
fn presentation_input_admits_only_the_reviewed_direct_action_once() {
    use festerm_automation::Error;
    use festerm_session::{AutomationInputStatus, CommitAutomationInputError};

    let _serial = presentation_admission_test_guard();
    let mut fixture = presented_input_fixture_with_binding(&[], true);
    let (wrong, mut rejected) = commit_direct_input_retry(
        &mut fixture.coordinator,
        1,
        fixture.request,
        &mut fixture.barrier,
        b"nope",
        fixture.app.automation_session(fixture.tab).unwrap(),
    );
    assert_eq!(
        wrong,
        Err(CommitAutomationInputError::Refused(Error::BindingMismatch))
    );
    assert!(matches!(rejected.poll(), AutomationInputStatus::Cancelled));
    let (wrong, mut rejected) = commit_presented_input_retry(
        &mut fixture.coordinator,
        1,
        fixture.request,
        &mut fixture.barrier,
        b"test",
    );
    assert_eq!(
        wrong,
        Err(CommitAutomationInputError::Refused(Error::BindingMismatch))
    );
    assert!(matches!(rejected.poll(), AutomationInputStatus::Cancelled));
    let target = fixture.barrier.target;
    let (wrong, mut rejected) = commit_direct_input_with_retry::<SimulatedPresentationBarrier>(
        &mut fixture.coordinator,
        1,
        fixture.request,
        None,
        |coordinator| reserve_presented_input(coordinator, target, b"test"),
        fixture.app.automation_session(fixture.tab).unwrap(),
    );
    assert_eq!(
        wrong,
        Err(CommitAutomationInputError::Refused(Error::BindingMismatch))
    );
    assert!(matches!(rejected.poll(), AutomationInputStatus::Cancelled));
    let (accepted, mut pending) = commit_direct_input_retry(
        &mut fixture.coordinator,
        1,
        fixture.request,
        &mut fixture.barrier,
        b"test",
        fixture.app.automation_session(fixture.tab).unwrap(),
    );
    accepted.unwrap();
    match pending.poll() {
        AutomationInputStatus::Ready(bytes) => assert_eq!(&*bytes, b"test"),
        _ => panic!("a successful admission must publish the approved bytes"),
    }
    let principal = fixture.coordinator.peers[&1].principal;
    let broker = fixture.coordinator.broker.as_mut().unwrap();
    assert_eq!(
        broker.prepare_request(
            principal,
            fixture.run,
            AuthorityOperation::Control {
                lease: fixture.lease,
                input: Digest(Sha256::digest(b"test").into()),
                bytes: 4,
                presentation: Some(fixture.requirement),
            },
            Instant::now(),
        ),
        Err(Error::Closed)
    );
    let status = broker
        .complete(
            principal,
            fixture.run,
            festerm_automation::Completion::Returned,
            Instant::now(),
        )
        .unwrap();
    assert_eq!(status.admitted_effects, 1);
    assert!(status.execution_outcome_unknown);
}

#[test]
fn direct_action_refuses_changed_modes_or_a_different_terminal() {
    use festerm_automation::Error;
    use festerm_session::{AutomationInputStatus, CommitAutomationInputError};

    let _serial = presentation_admission_test_guard();
    let mut fixture = presented_input_fixture_with_binding(&[], true);
    fixture
        .app
        .automation_session_mut(fixture.tab)
        .terminal
        .ingest(b"\x1b[?2004h");
    let (result, mut pending) = commit_direct_input_retry(
        &mut fixture.coordinator,
        1,
        fixture.request,
        &mut fixture.barrier,
        b"test",
        fixture.app.automation_session(fixture.tab).unwrap(),
    );
    assert_eq!(
        result,
        Err(CommitAutomationInputError::Refused(Error::BindingMismatch))
    );
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));

    let mut fixture = presented_input_fixture_with_binding(&[], true);
    let (_, other, other_tab, _) = self::fixture();
    let (result, mut pending) = commit_direct_input_retry(
        &mut fixture.coordinator,
        1,
        fixture.request,
        &mut fixture.barrier,
        b"test",
        other.automation_session(other_tab).unwrap(),
    );
    assert_eq!(
        result,
        Err(CommitAutomationInputError::Refused(Error::BindingMismatch))
    );
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
}

#[test]
fn direct_action_review_freezes_real_key_encoding_and_supersedes_old_action() {
    use festerm_automation::Error;
    use festerm_session::{AutomationInputStatus, CommitAutomationInputError};

    let _serial = presentation_admission_test_guard();
    let mut fixture = presented_input_fixture_with_binding(&[], true);
    let keys = vec!["ctrl:c".to_owned(), "alt:ArrowLeft".to_owned()];
    let target = fixture.barrier.target;
    let review = retry_automation_busy(|| {
        fixture.coordinator.prepare_direct_action_review(
            1,
            target,
            DirectAction::Keys(&keys),
            Duration::from_secs(60),
            fixture.app.automation_session_mut(fixture.tab),
        )
    })
    .unwrap();
    let binding = *review.request().bindings.first().unwrap();
    assert!(matches!(
        binding.invocation,
        Invocation::DirectInput { input, bytes: 7, .. }
            if input == Digest(Sha256::digest(b"\x03\x1b[1;3D").into())
    ));
    approve_control_review(&mut fixture.coordinator, 1, &review).unwrap();
    let (result, mut pending) = commit_direct_input_retry(
        &mut fixture.coordinator,
        1,
        fixture.request,
        &mut fixture.barrier,
        b"test",
        fixture.app.automation_session(fixture.tab).unwrap(),
    );
    assert!(matches!(
        result,
        Err(CommitAutomationInputError::Refused(
            Error::Revoked | Error::Closed | Error::BindingMismatch
        ))
    ));
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
}

#[test]
fn direct_action_review_refuses_invalid_or_oversize_input() {
    let _serial = presentation_admission_test_guard();
    let (mut coordinator, mut app, tab, _ime, target) = control_review_fixture();
    let large = "x".repeat(festerm_automation::MAX_INPUT_BYTES as usize + 1);
    let invalid = vec!["ctrl:ctrl:c".to_owned()];
    for action in [
        DirectAction::Text {
            text: &large,
            submit: false,
        },
        DirectAction::Keys(&invalid),
    ] {
        assert!(matches!(
            coordinator.prepare_direct_action_review(
                1,
                target,
                action,
                Duration::from_secs(60),
                app.automation_session_mut(tab),
            ),
            Err(festerm_automation::Error::InvalidRequest)
        ));
    }
    assert!(coordinator.pending_control_reviews.is_empty());
}

#[test]
fn direct_action_without_presentation_admits_core_bytes_once() {
    use festerm_automation::Error;
    use festerm_session::AutomationInputStatus;

    let _serial = presentation_admission_test_guard();
    let (mut coordinator, mut app, tab, _ime, target) = control_review_fixture();
    app.automation_session_mut(tab)
        .terminal
        .ingest(b"\x1b[?2004h");
    let review = retry_automation_busy(|| {
        coordinator.prepare_direct_action_review(
            1,
            target,
            DirectAction::Text {
                text: "hello",
                submit: true,
            },
            Duration::from_secs(60),
            app.automation_session_mut(tab),
        )
    })
    .unwrap();
    let binding = *review.request().bindings.first().unwrap();
    let scope = review.request().scope.clone();
    approve_control_review(&mut coordinator, 1, &review).unwrap();
    let principal = coordinator.peers[&1].principal;
    let now = Instant::now();
    let run = coordinator
        .broker
        .as_mut()
        .unwrap()
        .admit_run(
            principal,
            None,
            RunSpec {
                binding,
                scope,
                budget: Budget::default(),
                deadline: now + Duration::from_secs(30),
            },
            now,
        )
        .unwrap();
    let lease =
        retry_automation_busy(|| coordinator.acquire_control_lease(1, run, target)).unwrap();
    let input = b"\x1b[200~hello\x1b[201~\r";
    let operation = AuthorityOperation::Control {
        lease,
        input: Digest(Sha256::digest(input).into()),
        bytes: input.len() as u32,
        presentation: None,
    };
    let request = coordinator
        .broker
        .as_mut()
        .unwrap()
        .prepare_request(principal, run, operation, Instant::now())
        .unwrap();
    let (result, mut pending) = commit_direct_input_with_retry::<SimulatedPresentationBarrier>(
        &mut coordinator,
        1,
        request,
        None,
        |coordinator| reserve_presented_input(coordinator, target, input),
        app.automation_session(tab).unwrap(),
    );
    result.unwrap();
    assert!(matches!(pending.poll(), AutomationInputStatus::Ready(bytes) if &*bytes == input));
    assert_eq!(
        coordinator.broker.as_mut().unwrap().prepare_request(
            principal,
            run,
            operation,
            Instant::now(),
        ),
        Err(Error::Closed)
    );
}

#[test]
fn direct_action_after_publication_loss_is_unknown_and_not_replayed() {
    use festerm_automation::Error;
    use festerm_session::{AutomationInputStatus, CommitAutomationInputError};

    let _serial = presentation_admission_test_guard();
    let mut fixture = presented_input_fixture_with_binding(&[], true);
    fixture.barrier.fail_after_operation = true;
    let (result, mut pending) = commit_direct_input_retry(
        &mut fixture.coordinator,
        1,
        fixture.request,
        &mut fixture.barrier,
        b"test",
        fixture.app.automation_session(fixture.tab).unwrap(),
    );
    assert_eq!(result, Err(CommitAutomationInputError::LostAfterCommit));
    assert!(matches!(pending.poll(), AutomationInputStatus::Ready(bytes) if &*bytes == b"test"));
    let principal = fixture.coordinator.peers[&1].principal;
    assert_eq!(
        fixture
            .coordinator
            .broker
            .as_mut()
            .unwrap()
            .prepare_request(
                principal,
                fixture.run,
                AuthorityOperation::Control {
                    lease: fixture.lease,
                    input: Digest(Sha256::digest(b"test").into()),
                    bytes: 4,
                    presentation: Some(fixture.requirement),
                },
                Instant::now(),
            ),
        Err(Error::Closed)
    );
}

#[test]
fn direct_action_is_cut_by_pending_human_input() {
    use festerm_automation::Error;
    use festerm_session::AutomationInputStatus;
    use festerm_session::CommitAutomationInputError;

    let _serial = presentation_admission_test_guard();
    let mut fixture = presented_input_fixture_with_binding(&[], true);
    let _human = fixture
        .app
        .automation_session_mut(fixture.tab)
        .controller
        .begin_pending_human_input();
    let (result, mut pending) = commit_direct_input_retry(
        &mut fixture.coordinator,
        1,
        fixture.request,
        &mut fixture.barrier,
        b"test",
        fixture.app.automation_session(fixture.tab).unwrap(),
    );
    assert!(result.is_err());
    assert!(!matches!(
        result,
        Err(CommitAutomationInputError::Refused(Error::Busy))
    ));
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
}

#[test]
fn direct_action_reserves_frozen_bytes_in_backend_order() {
    use festerm_automation::Error;
    use festerm_session::CommitAutomationInputError;

    let _serial = presentation_admission_test_guard();
    retry_busy_case(|| {
        let mut fixture = presented_input_fixture_with_binding(&[], true);
        let tab = fixture.app.automation_session(fixture.tab).unwrap();
        let Some(crate::tabs::ApplicationSession::TestSsh(backend)) = tab.controller.session()
        else {
            panic!("expected controlled backend");
        };
        backend.enable_ordered_test_input();
        backend.enqueue_ordered_test_input(b"before");
        let reservation =
            reserve_direct_input_with_retry(&mut fixture.coordinator, 1, fixture.request, tab)
                .unwrap();
        backend.enqueue_ordered_test_input(b"after");
        assert_eq!(backend.drain_ordered_test_input(), vec![b"before".to_vec()]);
        assert!(backend.drain_ordered_test_input().is_empty());
        let result = fixture.coordinator.commit_direct_input(
            1,
            fixture.request,
            reservation,
            tab,
            Some(&mut fixture.barrier),
        );
        if matches!(
            result,
            Err(CommitAutomationInputError::Refused(Error::Busy))
        ) {
            assert_eq!(backend.drain_ordered_test_input(), vec![b"after".to_vec()]);
            return false;
        }
        result.unwrap();
        assert_eq!(
            backend.drain_ordered_test_input(),
            vec![b"test".to_vec(), b"after".to_vec()]
        );
        assert!(backend.drain_ordered_test_input().is_empty());
        true
    });
}

#[test]
fn preference_disables_fence_presented_reservation_and_composed_metadata() {
    use crate::automation::preferences::Feature;
    use crate::configuration_startup::{
        ConfigurationReloader, SessionNameConfigurationFixture, TestSaveFailure,
    };
    use festerm_automation::Error;
    use festerm_config::{Configuration, InterfaceSettings};
    use festerm_session::CommitAutomationInputError;

    let _serial = presentation_admission_test_guard();
    for feature in [Feature::Mcp, Feature::Javascript] {
        for failure in [
            None,
            Some(TestSaveFailure::BeforeWrite),
            Some(TestSaveFailure::AfterWrite),
        ] {
            retry_busy_case(|| {
                let mut fixture = presented_input_fixture_with_options(&[], true, true);
                let target = fixture.barrier.target;
                let principal = fixture.coordinator.peers[&1].principal;
                let metadata = prepare_metadata_review(&mut fixture.coordinator, 1, &[target]);
                approve_metadata_review(&mut fixture.coordinator, 1, &metadata).unwrap();
                let (_, _, delivery) = metadata_disclosure(&mut fixture.coordinator, 1, &[target]);
                assert!(delivery.is_live(principal, Instant::now()));

                let file = SessionNameConfigurationFixture::new();
                let configuration = Configuration::empty()
                    .with_interface_settings(
                        InterfaceSettings::DEFAULT
                            .with_mcp_enabled(true)
                            .with_javascript_enabled(true),
                    )
                    .unwrap();
                configuration.save_to_path(&file.path).unwrap();
                fixture.app.adopt_broadcast_configuration(configuration);
                fixture
                    .app
                    .set_reloader_for_test(ConfigurationReloader::from_path_for_test(
                        file.path.clone(),
                    ));
                fixture.app.initialize_automation_preferences();
                let settings = fixture.coordinator.settings(&[&fixture.app]);
                fixture.app.set_automation_settings(settings);

                let tab = fixture.app.automation_session(fixture.tab).unwrap();
                let Some(crate::tabs::ApplicationSession::TestSsh(backend)) =
                    tab.controller.session()
                else {
                    panic!("expected controlled backend");
                };
                let backend = backend.clone();
                backend.enable_ordered_test_input();
                backend.enqueue_ordered_test_input(b"before");
                let reservation = reserve_direct_input_with_retry(
                    &mut fixture.coordinator,
                    1,
                    fixture.request,
                    tab,
                )
                .unwrap();
                backend.enqueue_ordered_test_input(b"after");
                assert_eq!(backend.drain_ordered_test_input(), vec![b"before".to_vec()]);

                if let Some(failure) = failure {
                    fixture
                        .app
                        .shared_application_services()
                        .2
                        .fail_next_save_for_test(failure);
                }
                fixture
                    .app
                    .toggle_automation_preference_for_test(feature, &egui::Context::default());
                let should_stop = feature == Feature::Mcp || failure.is_some();
                assert_eq!(
                    fixture.coordinator.stop.load(Ordering::Acquire),
                    should_stop
                );
                let result = fixture.coordinator.commit_direct_input(
                    1,
                    fixture.request,
                    reservation,
                    fixture.app.automation_session(fixture.tab).unwrap(),
                    Some(&mut fixture.barrier),
                );
                if matches!(
                    result,
                    Err(CommitAutomationInputError::Refused(Error::Busy))
                ) {
                    assert_eq!(backend.drain_ordered_test_input(), vec![b"after".to_vec()]);
                    return false;
                }
                if should_stop {
                    assert_eq!(
                        result,
                        Err(CommitAutomationInputError::Refused(Error::Revoked))
                    );
                    assert_eq!(backend.drain_ordered_test_input(), vec![b"after".to_vec()]);
                    assert!(!delivery.is_live(principal, Instant::now()));
                } else {
                    assert_eq!(result, Ok(()));
                    assert_eq!(
                        backend.drain_ordered_test_input(),
                        vec![b"test".to_vec(), b"after".to_vec()]
                    );
                    assert!(delivery.is_live(principal, Instant::now()));
                }
                true
            });
        }
    }
}

#[test]
fn direct_action_reservation_refuses_stale_or_wrong_backend() {
    use festerm_automation::Error;
    use festerm_session::ReserveAutomationInputError;

    let _serial = presentation_admission_test_guard();
    let mut fixture = presented_input_fixture_with_binding(&[], true);
    let tab = fixture.app.automation_session(fixture.tab).unwrap();
    assert!(matches!(
        reserve_direct_input_with_retry(&mut fixture.coordinator, 1, fixture.request, tab),
        Err(ReserveDirectInputError::Backend(
            ReserveAutomationInputError::Unsupported
        ))
    ));
    let (_, other, other_tab, _) = self::fixture();
    assert!(matches!(
        reserve_direct_input_with_retry(
            &mut fixture.coordinator,
            1,
            fixture.request,
            other.automation_session(other_tab).unwrap()
        ),
        Err(ReserveDirectInputError::Authority(Error::BindingMismatch))
    ));
    fixture
        .app
        .automation_session_mut(fixture.tab)
        .terminal
        .ingest(b"\x1b[?2004h");
    assert!(matches!(
        reserve_direct_input_with_retry(
            &mut fixture.coordinator,
            1,
            fixture.request,
            fixture.app.automation_session(fixture.tab).unwrap()
        ),
        Err(ReserveDirectInputError::Authority(Error::BindingMismatch))
    ));
}

#[test]
fn direct_action_backend_reservation_rechecks_modes_at_publication() {
    use festerm_automation::Error;
    use festerm_session::CommitAutomationInputError;

    let _serial = presentation_admission_test_guard();
    retry_busy_case(|| {
        let mut fixture = presented_input_fixture_with_binding(&[], true);
        let tab = fixture.app.automation_session(fixture.tab).unwrap();
        let Some(crate::tabs::ApplicationSession::TestSsh(backend)) = tab.controller.session()
        else {
            panic!("expected controlled backend");
        };
        let backend = backend.clone();
        backend.enable_ordered_test_input();
        let reservation =
            reserve_direct_input_with_retry(&mut fixture.coordinator, 1, fixture.request, tab)
                .unwrap();
        backend.enqueue_ordered_test_input(b"after");
        fixture
            .app
            .automation_session_mut(fixture.tab)
            .terminal
            .ingest(b"\x1b[?2004h");
        let result = fixture.coordinator.commit_direct_input(
            1,
            fixture.request,
            reservation,
            fixture.app.automation_session(fixture.tab).unwrap(),
            Some(&mut fixture.barrier),
        );
        if matches!(
            result,
            Err(CommitAutomationInputError::Refused(Error::Busy))
        ) {
            assert_eq!(backend.drain_ordered_test_input(), vec![b"after".to_vec()]);
            return false;
        }
        assert_eq!(
            result,
            Err(CommitAutomationInputError::Refused(Error::BindingMismatch))
        );
        assert_eq!(backend.drain_ordered_test_input(), vec![b"after".to_vec()]);
        true
    });
}

#[test]
fn direct_action_reservation_discards_attachment_replaced_during_backend_call() {
    let _serial = presentation_admission_test_guard();
    let mut fixture = presented_input_fixture_with_binding(&[], true);
    let tab = fixture.app.automation_session(fixture.tab).unwrap();
    let target = &fixture.coordinator.targets[&fixture.barrier.target.session()];
    let attachment = target.attachment.clone();
    let generation = target.attachment_generation;
    let Some(crate::tabs::ApplicationSession::TestSsh(backend)) = tab.controller.session() else {
        panic!("expected controlled backend");
    };
    backend.enable_ordered_test_input();
    backend.invalidate_after_test_reservation();
    assert!(matches!(
        reserve_direct_input_with_retry(&mut fixture.coordinator, 1, fixture.request, tab),
        Err(ReserveDirectInputError::Authority(_))
    ));
    assert!(!attachment.is_current(generation));
    backend.enqueue_ordered_test_input(b"after");
    assert_eq!(backend.drain_ordered_test_input(), vec![b"after".to_vec()]);
}

#[test]
fn direct_action_metadata_first_requires_reviewed_closure_and_current_slot() {
    use festerm_automation::Error;
    use festerm_session::CommitAutomationInputError;

    let _serial = presentation_admission_test_guard();
    retry_busy_case(|| {
        let (source_app, source_tab) = FesTermApp::automation_fixture();
        let (mut coordinator, mut app, tab, _ime, target) =
            control_review_fixture_with_sources(&[&source_app]);
        let source = coordinator.targets[&source_tab.chip_id()].reference;
        let principal = coordinator.peers[&1].principal;
        let metadata = prepare_metadata_review(&mut coordinator, 1, &[source]);
        approve_metadata_review(&mut coordinator, 1, &metadata).unwrap();
        let (_, _, delivery) = metadata_disclosure(&mut coordinator, 1, &[source]);
        assert!(matches!(
            retry_automation_busy(|| coordinator.prepare_direct_action_review(
                1,
                target,
                DirectAction::Text {
                    text: "test",
                    submit: false
                },
                Duration::from_secs(60),
                app.automation_session_mut(tab),
            )),
            Err(Error::Unauthorized)
        ));
        assert!(delivery.is_live(principal, Instant::now()));
        let sources = Scope {
            discover: BTreeSet::from([source]),
            ..Scope::default()
        };
        let review = retry_automation_busy(|| {
            coordinator.prepare_direct_action_review_with_sources(
                1,
                target,
                DirectAction::Text {
                    text: "test",
                    submit: false,
                },
                Duration::from_secs(60),
                app.automation_session_mut(tab),
                &sources,
            )
        })
        .unwrap();
        assert_eq!(review.metadata_sources(), &BTreeSet::from([source]));
        assert_eq!(review.request().scope.discover, BTreeSet::from([source]));
        assert!(review.request().direct_read_calls.is_empty());
        let replacement = prepare_metadata_review(&mut coordinator, 1, &[source]);
        approve_metadata_review(&mut coordinator, 1, &replacement).unwrap();
        assert!(!delivery.is_live(principal, Instant::now()));
        assert_eq!(
            approve_control_review(&mut coordinator, 1, &review),
            Err(Error::Revoked)
        );
        let (_, _, current) = metadata_disclosure(&mut coordinator, 1, &[source]);
        let fresh = retry_automation_busy(|| {
            coordinator.prepare_direct_action_review_with_sources(
                1,
                target,
                DirectAction::Text {
                    text: "test",
                    submit: false,
                },
                Duration::from_secs(60),
                app.automation_session_mut(tab),
                &sources,
            )
        })
        .unwrap();
        assert_eq!(fresh.metadata_sources(), &BTreeSet::from([source]));
        let binding = *fresh.request().bindings.first().unwrap();
        let scope = fresh.request().scope.clone();
        approve_control_review(&mut coordinator, 1, &fresh).unwrap();
        let now = Instant::now();
        let run = coordinator
            .broker
            .as_mut()
            .unwrap()
            .admit_run(
                principal,
                None,
                RunSpec {
                    binding,
                    scope,
                    budget: Budget::default(),
                    deadline: now + Duration::from_secs(30),
                },
                now,
            )
            .unwrap();
        let lease =
            retry_automation_busy(|| coordinator.acquire_control_lease(1, run, target)).unwrap();
        let request = coordinator
            .broker
            .as_mut()
            .unwrap()
            .prepare_request(
                principal,
                run,
                AuthorityOperation::Control {
                    lease,
                    input: Digest(Sha256::digest(b"test").into()),
                    bytes: 4,
                    presentation: None,
                },
                Instant::now(),
            )
            .unwrap();
        let session = app.automation_session(tab).unwrap();
        let Some(crate::tabs::ApplicationSession::TestSsh(backend)) = session.controller.session()
        else {
            panic!("expected controlled backend");
        };
        backend.enable_ordered_test_input();
        let reservation =
            reserve_direct_input_with_retry(&mut coordinator, 1, request, session).unwrap();
        retry_automation_busy(|| coordinator.revoke_metadata_grant(1)).unwrap();
        assert!(!current.is_live(principal, Instant::now()));
        let result = coordinator.commit_direct_input::<SimulatedPresentationBarrier>(
            1,
            request,
            reservation,
            session,
            None,
        );
        if matches!(
            result,
            Err(CommitAutomationInputError::Refused(Error::Busy))
        ) {
            return false;
        }
        result.unwrap();
        assert_eq!(backend.drain_ordered_test_input(), vec![b"test".to_vec()]);
        assert!(reserve_direct_input_with_retry(&mut coordinator, 1, request, session).is_err());
        true
    });
}

#[test]
fn direct_action_reserved_input_is_cut_by_metadata_source_retirement() {
    use festerm_automation::Error;
    use festerm_session::CommitAutomationInputError;

    let _serial = presentation_admission_test_guard();
    retry_busy_case(|| {
        let (source_app, source_tab) = FesTermApp::automation_fixture();
        let mut fixture =
            presented_input_fixture_with_binding(&[(&source_app, source_tab, true)], true);
        let source = fixture.coordinator.targets[&source_tab.chip_id()].reference;
        let review = prepare_metadata_review(&mut fixture.coordinator, 1, &[source]);
        approve_metadata_review(&mut fixture.coordinator, 1, &review).unwrap();
        let principal = fixture.coordinator.peers[&1].principal;
        let (_, _, delivery) = metadata_disclosure(&mut fixture.coordinator, 1, &[source]);
        let tab = fixture.app.automation_session(fixture.tab).unwrap();
        let Some(crate::tabs::ApplicationSession::TestSsh(backend)) = tab.controller.session()
        else {
            panic!("expected controlled backend");
        };
        backend.enable_ordered_test_input();
        let reservation =
            reserve_direct_input_with_retry(&mut fixture.coordinator, 1, fixture.request, tab)
                .unwrap();
        replace_source_attachment_without_event(&source_app, source_tab);
        let result = fixture.coordinator.commit_direct_input(
            1,
            fixture.request,
            reservation,
            tab,
            Some(&mut fixture.barrier),
        );
        if matches!(
            result,
            Err(CommitAutomationInputError::Refused(Error::Busy))
        ) {
            return false;
        }
        assert!(result.is_err());
        assert!(backend.drain_ordered_test_input().is_empty());
        assert!(!delivery.is_live(principal, Instant::now()));
        true
    });
}

#[test]
fn direct_action_ordinary_expiry_cuts_presentation_and_reservation_not_metadata() {
    use festerm_automation::Error;
    use festerm_session::CommitAutomationInputError;

    let _serial = presentation_admission_test_guard();
    for reserve_before_expiry in [false, true] {
        retry_busy_case(|| {
            let (source_app, source_tab) = FesTermApp::automation_fixture();
            let mut fixture =
                presented_input_fixture_with_binding(&[(&source_app, source_tab, true)], true);
            let source = fixture.coordinator.targets[&source_tab.chip_id()].reference;
            let principal = fixture.coordinator.peers[&1].principal;
            let review = retry_automation_busy(|| {
                fixture.coordinator.prepare_metadata_review(
                    1,
                    festerm_automation::MetadataGrantRequest {
                        targets: BTreeSet::from([source]),
                    },
                    Duration::from_secs(120),
                )
            })
            .unwrap();
            approve_metadata_review(&mut fixture.coordinator, 1, &review).unwrap();
            let (_, _, delivery) = metadata_disclosure_with_deadline(
                &mut fixture.coordinator,
                1,
                &[source],
                Duration::from_secs(90),
            );
            let tab = fixture.app.automation_session(fixture.tab).unwrap();
            let Some(crate::tabs::ApplicationSession::TestSsh(backend)) = tab.controller.session()
            else {
                panic!("expected controlled backend");
            };
            backend.enable_ordered_test_input();
            let reservation = reserve_before_expiry.then(|| {
                reserve_direct_input_with_retry(&mut fixture.coordinator, 1, fixture.request, tab)
                    .unwrap()
            });
            let future = Instant::now() + Duration::from_secs(61);
            fixture
                .coordinator
                .broker
                .as_mut()
                .unwrap()
                .poll_expiry(future);
            if let Some(reservation) = reservation {
                let result = fixture.coordinator.commit_direct_input(
                    1,
                    fixture.request,
                    reservation,
                    tab,
                    Some(&mut fixture.barrier),
                );
                if matches!(
                    result,
                    Err(CommitAutomationInputError::Refused(Error::Busy))
                ) {
                    return false;
                }
                assert!(result.is_err());
            } else {
                assert!(reserve_direct_input_with_retry(
                    &mut fixture.coordinator,
                    1,
                    fixture.request,
                    tab
                )
                .is_err());
            }
            assert!(backend.drain_ordered_test_input().is_empty());
            assert!(delivery.is_live(principal, future));
            true
        });
    }
}

#[test]
fn presentation_metadata_revocation_preserves_pending_direct_input_without_replay() {
    use festerm_automation::{Error, RunState};
    use festerm_session::{AutomationInputStatus, CommitAutomationInputError};

    let _serial = presentation_admission_test_guard();
    let (source_app, source_tab) = FesTermApp::automation_fixture();
    let mut fixture =
        presented_input_fixture_with_binding(&[(&source_app, source_tab, true)], true);
    let principal = fixture.coordinator.peers[&1].principal;
    let source = fixture.coordinator.targets[&source_tab.chip_id()].reference;
    let review = prepare_metadata_review(&mut fixture.coordinator, 1, &[source]);
    assert_eq!(review.request().targets, BTreeSet::from([source]));
    assert_eq!(review.control_target(), Some(fixture.barrier.target));
    assert_eq!(review.control_sources(), &BTreeSet::from([source]));
    approve_metadata_review(&mut fixture.coordinator, 1, &review).unwrap();
    let (_, _, metadata) = metadata_disclosure(&mut fixture.coordinator, 1, &[source]);
    assert!(metadata.is_live(principal, Instant::now()));
    assert!(!fixture.coordinator.javascript_enabled);

    let (wrong_target, mut pending) = commit_direct_input_with_retry(
        &mut fixture.coordinator,
        1,
        fixture.request,
        Some(&mut fixture.barrier),
        |coordinator| reserve_presented_input(coordinator, source, b"test"),
        fixture.app.automation_session(fixture.tab).unwrap(),
    );
    assert_eq!(
        wrong_target,
        Err(CommitAutomationInputError::Refused(Error::BindingMismatch))
    );
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
    let (wrong_bytes, mut pending) = commit_direct_input_retry(
        &mut fixture.coordinator,
        1,
        fixture.request,
        &mut fixture.barrier,
        b"nope",
        fixture.app.automation_session(fixture.tab).unwrap(),
    );
    assert_eq!(
        wrong_bytes,
        Err(CommitAutomationInputError::Refused(Error::BindingMismatch))
    );
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));

    retry_automation_busy(|| fixture.coordinator.revoke_metadata_grant(1)).unwrap();
    assert!(!metadata.is_live(principal, Instant::now()));
    fixture.barrier.fail_after_operation = true;
    let (result, mut pending) = commit_direct_input_retry(
        &mut fixture.coordinator,
        1,
        fixture.request,
        &mut fixture.barrier,
        b"test",
        fixture.app.automation_session(fixture.tab).unwrap(),
    );
    assert_eq!(result, Err(CommitAutomationInputError::LostAfterCommit));
    assert!(matches!(
        pending.poll(),
        AutomationInputStatus::Ready(bytes) if &*bytes == b"test"
    ));
    let (replay, mut pending) = commit_direct_input_retry(
        &mut fixture.coordinator,
        1,
        fixture.request,
        &mut fixture.barrier,
        b"test",
        fixture.app.automation_session(fixture.tab).unwrap(),
    );
    assert!(replay.is_err());
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
    let broker = fixture.coordinator.broker.as_mut().unwrap();
    assert_eq!(
        broker.prepare_request(
            principal,
            fixture.run,
            AuthorityOperation::Control {
                lease: fixture.lease,
                input: Digest(Sha256::digest(b"test").into()),
                bytes: 4,
                presentation: Some(fixture.requirement),
            },
            Instant::now(),
        ),
        Err(Error::Closed)
    );
    broker.settle_request(principal, fixture.request).unwrap();
    let status = broker
        .complete(principal, fixture.run, Completion::Returned, Instant::now())
        .unwrap();
    assert_eq!(status.state, RunState::Completed);
    assert_eq!(status.admitted_effects, 1);
    assert!(status.execution_outcome_unknown);
}

#[test]
fn presentation_metadata_and_control_expire_independently() {
    use festerm_automation::MetadataGrantRequest;
    use festerm_session::AutomationInputStatus;

    let _serial = presentation_admission_test_guard();
    for metadata_expires in [true, false] {
        let (source_app, source_tab) = FesTermApp::automation_fixture();
        let mut fixture =
            presented_input_fixture_with_binding(&[(&source_app, source_tab, true)], true);
        let principal = fixture.coordinator.peers[&1].principal;
        let source = fixture.coordinator.targets[&source_tab.chip_id()].reference;
        let metadata_lifetime = Duration::from_secs(if metadata_expires { 10 } else { 120 });
        let review = retry_automation_busy(|| {
            fixture.coordinator.prepare_metadata_review(
                1,
                MetadataGrantRequest {
                    targets: BTreeSet::from([source]),
                },
                metadata_lifetime,
            )
        })
        .unwrap();
        approve_metadata_review(&mut fixture.coordinator, 1, &review).unwrap();
        let run_lifetime = Duration::from_secs(if metadata_expires { 5 } else { 90 });
        let (_, _, delivery) =
            metadata_disclosure_with_deadline(&mut fixture.coordinator, 1, &[source], run_lifetime);
        let future = Instant::now() + Duration::from_secs(if metadata_expires { 11 } else { 61 });
        let broker = fixture.coordinator.broker.as_mut().unwrap();
        broker.poll_expiry(future);
        assert_eq!(delivery.is_live(principal, future), !metadata_expires);
        assert_eq!(
            broker.native_metadata_grant(principal, future).is_ok(),
            !metadata_expires
        );
        assert_eq!(
            broker
                .native_control_grant(principal, fixture.barrier.target, future)
                .is_ok(),
            metadata_expires
        );

        let (result, mut pending) = commit_direct_input_retry(
            &mut fixture.coordinator,
            1,
            fixture.request,
            &mut fixture.barrier,
            b"test",
            fixture.app.automation_session(fixture.tab).unwrap(),
        );
        if metadata_expires {
            assert_eq!(result, Ok(()));
            assert!(matches!(
                pending.poll(),
                AutomationInputStatus::Ready(bytes) if &*bytes == b"test"
            ));
        } else {
            assert!(result.is_err());
            assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
            assert!(delivery.is_live(principal, future));
        }
    }
}

#[test]
fn presentation_metadata_first_requires_explicit_complete_control_review() {
    use festerm_automation::Error;

    let _serial = presentation_admission_test_guard();
    let (source_app, source_tab) = FesTermApp::automation_fixture();
    let (mut coordinator, mut app, tab, _ime, target) =
        control_review_fixture_with_sources(&[&source_app]);
    let source = coordinator.targets[&source_tab.chip_id()].reference;
    let principal = coordinator.peers[&1].principal;
    let metadata = prepare_metadata_review(&mut coordinator, 1, &[source]);
    assert_eq!(metadata.control_target(), None);
    assert!(metadata.control_sources().is_empty());
    approve_metadata_review(&mut coordinator, 1, &metadata).unwrap();
    let (_, _, liveness) = metadata_disclosure(&mut coordinator, 1, &[source]);

    let (mut request, _, mut binding) = presentation_control_request(target);
    binding.invocation = Invocation::DirectInput {
        target,
        input: Digest(Sha256::digest(b"test").into()),
        bytes: 4,
    };
    request.scope.invoke_workflows = false;
    request.scope.return_results = false;
    request.bindings = BTreeSet::from([binding]);
    assert!(matches!(
        retry_automation_busy(|| coordinator.prepare_control_handoff_review(
            1,
            target,
            request.clone(),
            Duration::from_secs(60),
            &mut app.automation_session_mut(tab).controller,
        )),
        Err(Error::Unauthorized)
    ));
    assert!(liveness.is_live(principal, Instant::now()));
    request.scope.discover.insert(source);
    let stale = prepare_control_review(
        &mut coordinator,
        1,
        target,
        &request,
        &mut app.automation_session_mut(tab).controller,
    );
    assert_eq!(stale.metadata_sources(), &BTreeSet::from([source]));
    assert!(!stale.request().scope.invoke_workflows);
    assert!(stale.request().scope.observe.is_empty());

    let replacement = prepare_metadata_review(&mut coordinator, 1, &[source]);
    approve_metadata_review(&mut coordinator, 1, &replacement).unwrap();
    assert!(!liveness.is_live(principal, Instant::now()));
    assert_eq!(
        approve_control_review(&mut coordinator, 1, &stale),
        Err(Error::Revoked)
    );
    let fresh = prepare_control_review(
        &mut coordinator,
        1,
        target,
        &request,
        &mut app.automation_session_mut(tab).controller,
    );
    assert_eq!(fresh.metadata_sources(), &BTreeSet::from([source]));
    approve_control_review(&mut coordinator, 1, &fresh).unwrap();
    assert!(coordinator
        .broker
        .as_ref()
        .unwrap()
        .native_control_grant(principal, target, Instant::now())
        .is_ok());
}

#[test]
fn presentation_metadata_reapproval_cannot_widen_frozen_control_sources() {
    use festerm_automation::{Error, MetadataGrantRequest};
    use festerm_session::AutomationInputStatus;

    let _serial = presentation_admission_test_guard();
    let (a_app, a_tab) = FesTermApp::automation_fixture();
    let (b_app, b_tab) = FesTermApp::automation_fixture();
    let mut fixture = presented_input_fixture_with_binding(
        &[(&a_app, a_tab, true), (&b_app, b_tab, false)],
        true,
    );
    let principal = fixture.coordinator.peers[&1].principal;
    let a = fixture.coordinator.targets[&a_tab.chip_id()].reference;
    let b = fixture.coordinator.targets[&b_tab.chip_id()].reference;
    let sink = fixture.barrier.target;
    let first = prepare_metadata_review(&mut fixture.coordinator, 1, &[a]);
    assert_eq!(first.control_sources(), &BTreeSet::from([a, b]));
    approve_metadata_review(&mut fixture.coordinator, 1, &first).unwrap();
    let (_, _, old_delivery) = metadata_disclosure(&mut fixture.coordinator, 1, &[a]);

    let replacement = prepare_metadata_review(&mut fixture.coordinator, 1, &[a, b]);
    assert_eq!(replacement.control_target(), Some(sink));
    assert_eq!(replacement.request().targets, BTreeSet::from([a, b]));
    assert_eq!(replacement.control_sources(), &BTreeSet::from([a, b]));
    approve_metadata_review(&mut fixture.coordinator, 1, &replacement).unwrap();
    assert!(!old_delivery.is_live(principal, Instant::now()));
    let (_, _, current_delivery) = metadata_disclosure(&mut fixture.coordinator, 1, &[a, b]);
    assert!(matches!(
        retry_automation_busy(|| fixture.coordinator.prepare_metadata_review(
            1,
            MetadataGrantRequest {
                targets: BTreeSet::from([sink]),
            },
            Duration::from_secs(60),
        )),
        Err(Error::Unauthorized)
    ));
    assert!(current_delivery.is_live(principal, Instant::now()));
    let (result, mut pending) = commit_direct_input_retry(
        &mut fixture.coordinator,
        1,
        fixture.request,
        &mut fixture.barrier,
        b"test",
        fixture.app.automation_session(fixture.tab).unwrap(),
    );
    assert_eq!(result, Ok(()));
    assert!(matches!(
        pending.poll(),
        AutomationInputStatus::Ready(bytes) if &*bytes == b"test"
    ));
}

#[test]
fn presentation_control_cuts_preserve_metadata_and_reclaim_stale_reviews() {
    use festerm_automation::{Error, MetadataGrantRequest};
    use festerm_session::{AutomationInputStatus, CommitAutomationInputError};

    let _serial = presentation_admission_test_guard();
    for takeover in [false, true] {
        let (source_app, source_tab) = FesTermApp::automation_fixture();
        let (other_app, other_tab) = FesTermApp::automation_fixture();
        let mut fixture = presented_input_fixture_with_binding(
            &[
                (&source_app, source_tab, true),
                (&other_app, other_tab, true),
            ],
            true,
        );
        let principal = fixture.coordinator.peers[&1].principal;
        let other_principal = add_metadata_peer(&mut fixture.coordinator, 2);
        let source = fixture.coordinator.targets[&source_tab.chip_id()].reference;
        let other = fixture.coordinator.targets[&other_tab.chip_id()].reference;
        let first = prepare_metadata_review(&mut fixture.coordinator, 1, &[source]);
        approve_metadata_review(&mut fixture.coordinator, 1, &first).unwrap();
        let (_, _, delivery) = metadata_disclosure(&mut fixture.coordinator, 1, &[source]);
        let other_first = prepare_metadata_review(&mut fixture.coordinator, 2, &[other]);
        approve_metadata_review(&mut fixture.coordinator, 2, &other_first).unwrap();
        let (_, _, other_delivery) = metadata_disclosure(&mut fixture.coordinator, 2, &[other]);
        let pending_a = prepare_metadata_review(&mut fixture.coordinator, 1, &[source]);
        let pending_b = prepare_metadata_review(&mut fixture.coordinator, 1, &[source]);
        let pending_other = prepare_metadata_review(&mut fixture.coordinator, 2, &[other]);
        assert_eq!(
            approve_metadata_review(&mut fixture.coordinator, 2, &pending_a),
            Err(Error::Unauthorized)
        );
        assert!(matches!(
            retry_automation_busy(|| fixture.coordinator.prepare_metadata_review(
                1,
                MetadataGrantRequest {
                    targets: BTreeSet::from([source]),
                },
                Duration::from_secs(60),
            )),
            Err(Error::QuotaExceeded)
        ));

        let (foreign, mut foreign_pending) = commit_direct_input_retry(
            &mut fixture.coordinator,
            2,
            fixture.request,
            &mut fixture.barrier,
            b"test",
            fixture.app.automation_session(fixture.tab).unwrap(),
        );
        assert!(matches!(
            foreign,
            Err(CommitAutomationInputError::Refused(Error::Unauthorized))
        ));
        assert!(matches!(
            foreign_pending.poll(),
            AutomationInputStatus::Cancelled
        ));
        if takeover {
            type_in_presentation_tab(&mut fixture.app, fixture.tab);
        } else {
            retry_automation_busy(|| fixture.coordinator.revoke_control_handoff(1)).unwrap();
        }
        let (result, mut pending) = commit_direct_input_retry(
            &mut fixture.coordinator,
            1,
            fixture.request,
            &mut fixture.barrier,
            b"test",
            fixture.app.automation_session(fixture.tab).unwrap(),
        );
        assert!(result.is_err());
        assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
        assert!(delivery.is_live(principal, Instant::now()));
        assert!(other_delivery.is_live(other_principal, Instant::now()));
        assert_eq!(
            approve_metadata_review(&mut fixture.coordinator, 1, &pending_a),
            Err(Error::Revoked)
        );
        assert_eq!(
            approve_metadata_review(&mut fixture.coordinator, 1, &pending_b),
            Err(Error::Revoked)
        );
        approve_metadata_review(&mut fixture.coordinator, 2, &pending_other).unwrap();
        let fresh = prepare_metadata_review(&mut fixture.coordinator, 1, &[source]);
        assert_eq!(fresh.control_target(), None);
        assert!(fixture
            .coordinator
            .broker
            .as_ref()
            .unwrap()
            .prepare_lease(
                principal,
                fixture.run,
                fixture.barrier.target,
                Instant::now()
            )
            .is_err());
    }
}

#[test]
fn presentation_takeover_preserves_metadata_for_the_same_exact_target() {
    use festerm_session::AutomationInputStatus;

    let _serial = presentation_admission_test_guard();
    let mut fixture = presented_input_fixture_with_options(&[], true, true);
    let principal = fixture.coordinator.peers[&1].principal;
    let target = fixture.barrier.target;
    let review = prepare_metadata_review(&mut fixture.coordinator, 1, &[target]);
    assert_eq!(review.control_target(), Some(target));
    approve_metadata_review(&mut fixture.coordinator, 1, &review).unwrap();
    let (_, _, delivery) = metadata_disclosure(&mut fixture.coordinator, 1, &[target]);

    type_in_presentation_tab(&mut fixture.app, fixture.tab);
    let (result, mut pending) = commit_direct_input_retry(
        &mut fixture.coordinator,
        1,
        fixture.request,
        &mut fixture.barrier,
        b"test",
        fixture.app.automation_session(fixture.tab).unwrap(),
    );
    assert!(result.is_err());
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
    assert!(delivery.is_live(principal, Instant::now()));
    assert!(metadata_disclosure(&mut fixture.coordinator, 1, &[target])
        .2
        .is_live(principal, Instant::now()));
    assert!(fixture
        .coordinator
        .broker
        .as_ref()
        .unwrap()
        .prepare_lease(principal, fixture.run, target, Instant::now())
        .is_err());
}

#[test]
fn presentation_metadata_review_and_slot_cuts_never_wait_for_admission() {
    use festerm_automation::{Error, MetadataGrantRequest};
    use festerm_session::AutomationInputStatus;

    let _serial = presentation_admission_test_guard();
    let (source_app, source_tab) = FesTermApp::automation_fixture();
    let mut fixture =
        presented_input_fixture_with_binding(&[(&source_app, source_tab, true)], true);
    let source = fixture.coordinator.targets[&source_tab.chip_id()].reference;
    let review = prepare_metadata_review(&mut fixture.coordinator, 1, &[source]);
    let decision = review.accept_from_native();
    let guard = festerm_session::automation_admission_lock().unwrap();
    assert!(matches!(
        fixture.coordinator.prepare_metadata_review(
            1,
            MetadataGrantRequest {
                targets: BTreeSet::from([source]),
            },
            Duration::from_secs(60),
        ),
        Err(Error::Busy)
    ));
    assert_eq!(
        fixture
            .coordinator
            .approve_metadata_review(1, &review, &decision),
        Err(Error::Busy)
    );
    assert_eq!(
        fixture.coordinator.cancel_metadata_review(1, &review),
        Err(Error::Busy)
    );
    assert_eq!(
        fixture.coordinator.revoke_control_handoff(1),
        Err(Error::Busy)
    );
    assert_eq!(
        fixture.coordinator.revoke_metadata_grant(1),
        Err(Error::Busy)
    );
    drop(guard);
    approve_metadata_review(&mut fixture.coordinator, 1, &review).unwrap();
    let (result, mut pending) = commit_direct_input_retry(
        &mut fixture.coordinator,
        1,
        fixture.request,
        &mut fixture.barrier,
        b"test",
        fixture.app.automation_session(fixture.tab).unwrap(),
    );
    assert_eq!(result, Ok(()));
    assert!(matches!(pending.poll(), AutomationInputStatus::Ready(_)));
}

#[test]
fn presentation_metadata_sources_are_frozen_before_consent_and_through_publication() {
    use festerm_session::AutomationInputStatus;

    let _serial = presentation_admission_test_guard();
    for after_consent in [false, true] {
        for change in 0..6 {
            let (mut source_app, source_tab) = FesTermApp::automation_fixture();
            let mut fixture =
                presented_input_fixture_with_binding(&[(&source_app, source_tab, true)], true);
            let principal = fixture.coordinator.peers[&1].principal;
            let source = fixture.coordinator.targets[&source_tab.chip_id()].reference;
            let review = prepare_metadata_review(&mut fixture.coordinator, 1, &[source]);
            let delivery = if after_consent {
                approve_metadata_review(&mut fixture.coordinator, 1, &review).unwrap();
                Some(metadata_disclosure(&mut fixture.coordinator, 1, &[source]).2)
            } else {
                None
            };
            change_readable_source(
                &mut source_app,
                source_tab,
                &mut fixture.coordinator,
                change,
            );
            if !after_consent {
                assert!(approve_metadata_review(&mut fixture.coordinator, 1, &review).is_err());
            }
            let (result, mut pending) = commit_direct_input_retry(
                &mut fixture.coordinator,
                1,
                fixture.request,
                &mut fixture.barrier,
                b"test",
                fixture.app.automation_session(fixture.tab).unwrap(),
            );
            assert!(result.is_err());
            assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
            if let Some(delivery) = delivery {
                assert!(!delivery.is_live(principal, Instant::now()));
            }
            let broker = fixture.coordinator.broker.as_ref().unwrap();
            assert!(broker
                .native_metadata_grant(principal, Instant::now())
                .is_err());
            assert!(broker
                .native_control_grant(principal, fixture.barrier.target, Instant::now())
                .is_err());
        }
    }
}

#[test]
fn presentation_unbound_metadata_replacement_cuts_control_without_revoking_metadata() {
    use festerm_automation::{MetadataGrantRequest, NativeMetadataApproval};
    use festerm_session::AutomationInputStatus;

    let _serial = presentation_admission_test_guard();
    let (source_app, source_tab) = FesTermApp::automation_fixture();
    let mut fixture =
        presented_input_fixture_with_binding(&[(&source_app, source_tab, true)], true);
    let principal = fixture.coordinator.peers[&1].principal;
    let source = fixture.coordinator.targets[&source_tab.chip_id()].reference;
    let review = prepare_metadata_review(&mut fixture.coordinator, 1, &[source]);
    approve_metadata_review(&mut fixture.coordinator, 1, &review).unwrap();
    let (_, _, old_delivery) = metadata_disclosure(&mut fixture.coordinator, 1, &[source]);

    // Simulate an adapter bypassing the host's frozen review seam.
    let now = Instant::now();
    let request = MetadataGrantRequest {
        targets: BTreeSet::from([source]),
    };
    let broker = fixture.coordinator.broker.as_mut().unwrap();
    let id = broker
        .request_metadata_approval(principal, request.clone(), now)
        .unwrap();
    broker
        .approve_metadata(
            principal,
            id,
            NativeMetadataApproval::new(request.clone(), Duration::from_secs(60))
                .accept_control_disclosure(fixture.barrier.target, request.targets),
            now,
        )
        .unwrap();
    let unbound = broker.native_metadata_grant(principal, now).unwrap();
    assert!(!old_delivery.is_live(principal, now));
    let (result, mut pending) = commit_direct_input_retry(
        &mut fixture.coordinator,
        1,
        fixture.request,
        &mut fixture.barrier,
        b"test",
        fixture.app.automation_session(fixture.tab).unwrap(),
    );
    assert!(result.is_err());
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
    let broker = fixture.coordinator.broker.as_ref().unwrap();
    assert_eq!(
        broker.native_metadata_grant(principal, Instant::now()),
        Ok(unbound)
    );
    assert!(broker
        .native_control_grant(principal, fixture.barrier.target, Instant::now())
        .is_err());

    let fresh = prepare_metadata_review(&mut fixture.coordinator, 1, &[source]);
    assert_eq!(fresh.control_target(), None);
    approve_metadata_review(&mut fixture.coordinator, 1, &fresh).unwrap();
    assert_ne!(
        fixture
            .coordinator
            .broker
            .as_ref()
            .unwrap()
            .native_metadata_grant(principal, Instant::now()),
        Ok(unbound)
    );
}

#[test]
fn presentation_metadata_privacy_and_disconnect_cut_the_right_principals() {
    use festerm_automation::{Error, MetadataRunSpec};
    use festerm_session::AutomationInputStatus;

    let _serial = presentation_admission_test_guard();
    for privacy in [false, true] {
        let (source_app, source_tab) = FesTermApp::automation_fixture();
        let (other_app, other_tab) = FesTermApp::automation_fixture();
        let mut fixture = presented_input_fixture_with_binding(
            &[
                (&source_app, source_tab, true),
                (&other_app, other_tab, true),
            ],
            true,
        );
        let principal = fixture.coordinator.peers[&1].principal;
        let other_principal = add_metadata_peer(&mut fixture.coordinator, 2);
        let source = fixture.coordinator.targets[&source_tab.chip_id()].reference;
        let other = fixture.coordinator.targets[&other_tab.chip_id()].reference;
        let first = prepare_metadata_review(&mut fixture.coordinator, 1, &[source]);
        approve_metadata_review(&mut fixture.coordinator, 1, &first).unwrap();
        let (_, _, delivery) = metadata_disclosure(&mut fixture.coordinator, 1, &[source]);
        let second = prepare_metadata_review(&mut fixture.coordinator, 2, &[other]);
        approve_metadata_review(&mut fixture.coordinator, 2, &second).unwrap();
        let (_, _, other_delivery) = metadata_disclosure(&mut fixture.coordinator, 2, &[other]);
        if privacy {
            fixture.coordinator.privacy_gate.close();
        } else {
            fixture.coordinator.peers[&1].connection.close();
        }
        let (result, mut pending) = commit_direct_input_retry(
            &mut fixture.coordinator,
            1,
            fixture.request,
            &mut fixture.barrier,
            b"test",
            fixture.app.automation_session(fixture.tab).unwrap(),
        );
        assert!(result.is_err());
        assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
        assert!(!delivery.is_live(principal, Instant::now()));
        assert_eq!(
            other_delivery.is_live(other_principal, Instant::now()),
            !privacy
        );
        if !privacy {
            let successor = add_metadata_peer(&mut fixture.coordinator, 3);
            let now = Instant::now();
            let broker = fixture.coordinator.broker.as_mut().unwrap();
            assert_eq!(
                broker.admit_metadata_run(
                    successor,
                    MetadataRunSpec {
                        parameters: Digest([19; 32]),
                        targets: BTreeSet::from([source]),
                        budget: Budget::default(),
                        deadline: now + Duration::from_secs(30),
                    },
                    now,
                ),
                Err(Error::Revoked)
            );
            assert!(broker
                .prepared_control_binding(successor, fixture.request, now)
                .is_err());
        }
    }
}

#[test]
fn presentation_metadata_does_not_compose_with_legacy_observation_delivery() {
    use festerm_automation::{Error, MetadataGrantRequest};

    let _serial = presentation_admission_test_guard();
    let (mut coordinator, mut app, tab, _ime, target) = control_review_fixture();
    coordinator.approve(1, vec![target]).unwrap();
    assert_eq!(
        retry_automation_busy(|| coordinator.revoke_control_handoff(1)),
        Err(Error::Unavailable)
    );
    assert!(coordinator.peers[&1].grant.is_some());
    assert!(matches!(
        retry_automation_busy(|| coordinator.prepare_metadata_review(
            1,
            MetadataGrantRequest {
                targets: BTreeSet::from([target]),
            },
            Duration::from_secs(60),
        )),
        Err(Error::Unavailable)
    ));
    let (request, _, _) = presentation_control_request(target);
    assert!(matches!(
        retry_automation_busy(|| coordinator.prepare_control_handoff_review(
            1,
            target,
            request.clone(),
            Duration::from_secs(60),
            &mut app.automation_session_mut(tab).controller,
        )),
        Err(Error::Unavailable)
    ));

    let (mut coordinator, _app, _tab, _ime, target) = control_review_fixture();
    let principal = coordinator.peers[&1].principal;
    let review = prepare_metadata_review(&mut coordinator, 1, &[target]);
    approve_metadata_review(&mut coordinator, 1, &review).unwrap();
    let (_, _, liveness) = metadata_disclosure(&mut coordinator, 1, &[target]);
    assert_eq!(
        coordinator.approve(1, vec![target]),
        Err(Refusal::NotAuthorized)
    );
    assert!(liveness.is_live(principal, Instant::now()));
}

#[test]
fn presentation_input_cuts_grant_when_any_unreconciled_source_changes() {
    use festerm_automation::{Error, RunState};
    use festerm_session::{AutomationInputStatus, CommitAutomationInputError};

    let _serial = presentation_admission_test_guard();
    for discover in [false, true] {
        for change in 0..6 {
            let (mut changed_app, changed_tab) = FesTermApp::automation_fixture();
            let (other_app, other_tab) = FesTermApp::automation_fixture();
            let mut fixture = presented_input_fixture_with_sources(&[
                (&changed_app, changed_tab, discover),
                (&other_app, other_tab, !discover),
            ]);
            let target = fixture.barrier.target;
            let principal = fixture.coordinator.peers[&1].principal;
            let changed = fixture.coordinator.targets[&changed_tab.chip_id()].reference;
            let other = fixture.coordinator.targets[&other_tab.chip_id()].reference;
            let registration = fixture.coordinator.targets[&changed_tab.chip_id()]
                .registration
                .clone();
            let (observer, observer_run, _) = independent_run(&mut fixture.coordinator, other);
            let first_reservation = reserve_presented_input(&fixture.coordinator, target, b"test");

            change_readable_source(
                &mut changed_app,
                changed_tab,
                &mut fixture.coordinator,
                change,
            );
            assert_eq!(
                fixture.coordinator.targets[&changed_tab.chip_id()].reference,
                changed
            );
            assert_eq!(
                fixture.coordinator.targets[&fixture.tab.chip_id()].reference,
                target
            );
            let target_is_live =
                festerm_session::automation_admission_lock().is_some_and(|_guard| {
                    let current = &fixture.coordinator.targets[&fixture.tab.chip_id()];
                    current.local_pty && current.guard().is_valid()
                });
            assert!(target_is_live);
            let mut first_reservation = Some(first_reservation);
            let (result, mut pending) = commit_presented_input_with_retry(
                &mut fixture.coordinator,
                1,
                fixture.request,
                &mut fixture.barrier,
                |coordinator| {
                    first_reservation
                        .take()
                        .unwrap_or_else(|| reserve_presented_input(coordinator, target, b"test"))
                },
            );
            assert!(matches!(
                result,
                Err(CommitAutomationInputError::Refused(
                    Error::Closed | Error::Revoked
                ))
            ));
            assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
            assert!(!registration.load(Ordering::Acquire));
            assert!(!fixture
                .coordinator
                .targets
                .contains_key(&changed_tab.chip_id()));
            let broker = fixture.coordinator.broker.as_mut().unwrap();
            assert_eq!(
                broker.native_control_grant(principal, target, Instant::now()),
                Err(Error::Revoked)
            );
            assert!(broker
                .prepared_control_binding(principal, fixture.request, Instant::now())
                .is_err());
            assert_eq!(
                broker
                    .status(observer, observer_run, Instant::now())
                    .unwrap()
                    .state,
                RunState::Running
            );
        }
    }
}

#[test]
fn presentation_source_loss_preserves_other_controller_without_that_source() {
    use festerm_automation::{Error, RunState};
    use festerm_session::AutomationInputStatus;

    let _serial = presentation_admission_test_guard();
    let (source_app, source_tab) = FesTermApp::automation_fixture();
    let mut fixture = presented_input_fixture_with_sources(&[(&source_app, source_tab, false)]);
    let target = fixture.barrier.target;
    let principal_one = fixture.coordinator.peers[&1].principal;
    let principal_two = fixture
        .coordinator
        .broker
        .as_mut()
        .unwrap()
        .accept_connection()
        .unwrap();
    let (connection, _receive) = ipc::test_connection(2);
    fixture.coordinator.peers.insert(
        2,
        Peer {
            connection,
            principal: principal_two,
            machine: ready(),
            code: VerificationCode::new("0123456789abcdef").unwrap(),
            grant: None,
            waits: Vec::new(),
            controller: controller::PeerState::default(),
        },
    );
    let (request, scope, binding) = presentation_control_request(target);
    let review = prepare_control_review(
        &mut fixture.coordinator,
        2,
        target,
        &request,
        &mut fixture.app.automation_session_mut(fixture.tab).controller,
    );
    approve_control_review(&mut fixture.coordinator, 2, &review).unwrap();
    let now = Instant::now();
    let broker = fixture.coordinator.broker.as_mut().unwrap();
    let other_grant = broker
        .native_control_grant(principal_two, target, now)
        .unwrap();
    let other_run = broker
        .admit_run(
            principal_two,
            None,
            RunSpec {
                binding,
                scope,
                budget: Budget::default(),
                deadline: now + Duration::from_secs(30),
            },
            now,
        )
        .unwrap();

    disconnect_source_without_reconciliation(&source_app, source_tab);
    let (result, mut pending) = commit_presented_input_retry(
        &mut fixture.coordinator,
        1,
        fixture.request,
        &mut fixture.barrier,
        b"test",
    );
    assert!(result.is_err());
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
    let broker = fixture.coordinator.broker.as_mut().unwrap();
    assert_eq!(
        broker.native_control_grant(principal_one, target, Instant::now()),
        Err(Error::Revoked)
    );
    assert_eq!(
        broker.native_control_grant(principal_two, target, Instant::now()),
        Ok(other_grant)
    );
    assert_eq!(
        broker
            .status(principal_two, other_run, Instant::now())
            .unwrap()
            .state,
        RunState::Running
    );
    assert_eq!(
        retry_automation_busy(|| fixture
            .coordinator
            .acquire_control_lease(2, other_run, target))
        .unwrap()
        .target(),
        target
    );
}

struct ConcurrentSourceTransitionBarrier {
    inner: SimulatedPresentationBarrier,
    start: mpsc::SyncSender<()>,
    checked: mpsc::Receiver<bool>,
    finished: mpsc::Receiver<()>,
    saw_busy: bool,
    finished_before_return: bool,
}

impl NativePresentationBarrier for ConcurrentSourceTransitionBarrier {
    fn with_observation<T>(
        &mut self,
        broker: &mut Broker,
        workspace_revision: u64,
        operation: impl FnOnce(&mut Broker, festerm_automation::NativePresentationObservation) -> T,
    ) -> festerm_automation::Result<T> {
        self.start.send(()).unwrap();
        self.saw_busy = self
            .checked
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or(false);
        self.finished_before_return = self.finished.try_recv().is_ok();
        let result = self
            .inner
            .with_observation(broker, workspace_revision, operation);
        self.finished_before_return |= self.finished.try_recv().is_ok();
        result
    }
}

#[test]
fn presentation_metadata_source_transition_is_ordered_after_direct_publication() {
    use festerm_session::{AutomationAdmissionTryError, AutomationInputStatus};

    let _serial = presentation_admission_test_guard();
    let (source_app, source_tab) = FesTermApp::automation_fixture();
    let mut fixture =
        presented_input_fixture_with_binding(&[(&source_app, source_tab, true)], true);
    let source = fixture.coordinator.targets[&source_tab.chip_id()].reference;
    let principal = fixture.coordinator.peers[&1].principal;
    let review = prepare_metadata_review(&mut fixture.coordinator, 1, &[source]);
    approve_metadata_review(&mut fixture.coordinator, 1, &review).unwrap();
    let (_, _, delivery) = metadata_disclosure(&mut fixture.coordinator, 1, &[source]);
    let source_epoch = fixture.coordinator.targets[&source_tab.chip_id()]
        .attachment
        .clone();
    let (start, started) = mpsc::sync_channel(1);
    let (checked_send, checked) = mpsc::sync_channel(1);
    let (finished_send, finished) = mpsc::sync_channel(1);
    let transition = std::thread::spawn(move || {
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        checked_send
            .send(matches!(
                festerm_session::try_automation_admission_lock(),
                Err(AutomationAdmissionTryError::Busy)
            ))
            .unwrap();
        source_epoch.transition(false);
        finished_send.send(()).unwrap();
    });
    let PresentedInputFixture {
        mut coordinator,
        app,
        tab,
        request,
        barrier,
        ..
    } = fixture;
    let target = barrier.target;
    let mut barrier = ConcurrentSourceTransitionBarrier {
        inner: barrier,
        start,
        checked,
        finished,
        saw_busy: false,
        finished_before_return: false,
    };
    let (result, mut pending) = commit_direct_input_with_retry(
        &mut coordinator,
        1,
        request,
        Some(&mut barrier),
        |host| reserve_presented_input(host, target, b"test"),
        app.automation_session(tab).unwrap(),
    );
    assert_eq!(result, Ok(()));
    assert!(matches!(pending.poll(), AutomationInputStatus::Ready(_)));
    transition.join().unwrap();
    assert!(barrier.saw_busy);
    assert!(!barrier.finished_before_return);
    barrier
        .finished
        .recv_timeout(Duration::from_secs(2))
        .unwrap();

    let (replay, mut pending) = commit_direct_input_retry(
        &mut coordinator,
        1,
        request,
        &mut barrier.inner,
        b"test",
        app.automation_session(tab).unwrap(),
    );
    assert!(replay.is_err());
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
    assert!(!delivery.is_live(principal, Instant::now()));
}

#[test]
fn presentation_input_orders_source_transition_after_publication() {
    use festerm_session::{AutomationAdmissionTryError, AutomationInputStatus};

    let _serial = presentation_admission_test_guard();
    let (source_app, source_tab) = FesTermApp::automation_fixture();
    let PresentedInputFixture {
        mut coordinator,
        app: _app,
        run,
        lease,
        requirement,
        request,
        barrier,
        ..
    } = presented_input_fixture_with_sources(&[(&source_app, source_tab, false)]);
    let principal = coordinator.peers[&1].principal;
    let second_request = coordinator
        .broker
        .as_mut()
        .unwrap()
        .prepare_request(
            principal,
            run,
            AuthorityOperation::Control {
                lease,
                input: Digest(Sha256::digest(b"test").into()),
                bytes: 4,
                presentation: Some(requirement),
            },
            Instant::now(),
        )
        .unwrap();
    let source_epoch = coordinator.targets[&source_tab.chip_id()]
        .attachment
        .clone();
    let (start, started) = mpsc::sync_channel(1);
    let (checked_send, checked) = mpsc::sync_channel(1);
    let (finished_send, finished) = mpsc::sync_channel(1);
    let transition = std::thread::spawn(move || {
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        let busy = matches!(
            festerm_session::try_automation_admission_lock(),
            Err(AutomationAdmissionTryError::Busy)
        );
        checked_send.send(busy).unwrap();
        source_epoch.transition(false);
        finished_send.send(()).unwrap();
    });
    let target = barrier.target;
    let mut barrier = ConcurrentSourceTransitionBarrier {
        inner: barrier,
        start,
        checked,
        finished,
        saw_busy: false,
        finished_before_return: false,
    };
    let (result, mut pending) = commit_presented_input_with_retry(
        &mut coordinator,
        1,
        request,
        &mut barrier,
        |coordinator| reserve_presented_input(coordinator, target, b"test"),
    );
    result.unwrap();
    assert!(matches!(pending.poll(), AutomationInputStatus::Ready(_)));
    transition.join().unwrap();
    assert!(barrier.saw_busy);
    assert!(!barrier.finished_before_return);
    barrier
        .finished
        .recv_timeout(Duration::from_secs(2))
        .unwrap();

    let (result, mut pending) = commit_presented_input_retry(
        &mut coordinator,
        1,
        second_request,
        &mut barrier.inner,
        b"test",
    );
    assert!(result.is_err());
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
}

#[test]
fn presentation_input_admission_disarms_on_human_ime_privacy_disconnect_and_replacement() {
    use festerm_session::AutomationInputStatus;

    let _serial = presentation_admission_test_guard();
    for transition in 0..5 {
        let PresentedInputFixture {
            mut coordinator,
            mut app,
            tab,
            ime,
            request,
            mut barrier,
            ..
        } = presented_input_fixture();
        match transition {
            0 => assert!(app
                .automation_session_mut(tab)
                .controller
                .begin_clipboard_input(77)),
            1 => {
                ime.transition(false);
                ime.transition(true);
            }
            2 => coordinator.privacy_gate.close(),
            3 => coordinator.peers[&1].connection.close(),
            4 => {
                let attachment = app
                    .automation_session_mut(tab)
                    .controller
                    .automation_attachment_epoch()
                    .unwrap();
                attachment.transition(false);
                attachment.transition(true);
            }
            _ => unreachable!(),
        }
        let (result, mut pending) =
            commit_presented_input_retry(&mut coordinator, 1, request, &mut barrier, b"test");
        assert!(result.is_err());
        assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
        let principal = coordinator.peers[&1].principal;
        assert!(coordinator
            .broker
            .as_ref()
            .unwrap()
            .prepared_control_target(principal, request, Instant::now())
            .is_err());
    }
}

#[test]
fn presentation_input_admission_refuses_aba_unknown_and_nonlocal_backends() {
    use festerm_session::AutomationInputStatus;

    let _serial = presentation_admission_test_guard();
    for transition in 0..7 {
        let PresentedInputFixture {
            mut coordinator,
            app: _app,
            tab,
            workspace,
            request,
            mut barrier,
            ..
        } = presented_input_fixture();
        match transition {
            0 => {
                workspace.changed();
                workspace.changed();
            }
            1 => {
                let broker = coordinator.broker.as_mut().unwrap();
                broker
                    .note_presentation_transition(barrier.monitor)
                    .unwrap();
                broker
                    .note_presentation_transition(barrier.monitor)
                    .unwrap();
            }
            2 => barrier.unavailable = true,
            3 => {
                coordinator
                    .targets
                    .get_mut(&tab.chip_id())
                    .unwrap()
                    .local_pty = false
            }
            4 => barrier.change_before_operation = Some(workspace.clone()),
            5 => assert_eq!(
                coordinator.bind_presentation_workspace(&WorkspaceRevision::default()),
                Err(festerm_automation::Error::Unavailable)
            ),
            6 => coordinator.presentation_workspace = None,
            _ => unreachable!(),
        }
        let (result, mut pending) =
            commit_presented_input_retry(&mut coordinator, 1, request, &mut barrier, b"test");
        assert!(result.is_err());
        assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
        if matches!(transition, 2 | 4..=6) {
            assert_eq!(
                coordinator.broker.as_mut().unwrap().observe_presentation(
                    barrier.monitor,
                    festerm_automation::NativePresentationState::Inactive,
                ),
                Err(festerm_automation::Error::Unavailable)
            );
        }
    }
}

fn type_in_presentation_tab(app: &mut FesTermApp, tab: TabId) {
    let session = app.automation_session_mut(tab);
    festerm_ui_egui::route_input(
        &mut session.terminal,
        festerm_core::InputEvent::Key(festerm_core::Key::Enter),
        &mut session.controller,
    );
}

#[test]
fn presentation_control_review_invalidates_before_first_grant_on_human_ime_and_attachment() {
    use festerm_automation::Error;

    let _serial = presentation_admission_test_guard();
    for transition in 0..8 {
        let (mut coordinator, mut app, tab, ime, target) = control_review_fixture();
        let principal = coordinator.peers[&1].principal;
        let (request, _, _) = presentation_control_request(target);
        let review = prepare_control_review(
            &mut coordinator,
            1,
            target,
            &request,
            &mut app.automation_session_mut(tab).controller,
        );
        assert_eq!(review.target(), target);
        assert_eq!(review.request(), &request);
        assert_eq!(review.lifetime(), Duration::from_secs(60));
        let decision = review.accept_from_native();
        let approval_id = *coordinator.pending_control_reviews.keys().next().unwrap();
        assert!(coordinator.control_handoffs.is_empty());
        let mut intent = None;
        match transition {
            0 => type_in_presentation_tab(&mut app, tab),
            1 => ime.transition(false),
            2 => {
                ime.transition(false);
                ime.transition(true);
            }
            3 | 4 => {
                let controller = &mut app.automation_session_mut(tab).controller;
                assert!(controller.begin_clipboard_input(77));
                if transition == 4 {
                    controller.cancel_clipboard_input(77, "test-cancel");
                }
            }
            5 | 6 => {
                intent = Some(
                    app.automation_session_mut(tab)
                        .controller
                        .begin_pending_human_input(),
                );
                if transition == 6 {
                    drop(intent.take());
                }
            }
            7 => {
                let attachment = app
                    .automation_session_mut(tab)
                    .controller
                    .automation_attachment_epoch()
                    .unwrap();
                attachment.transition(false);
                attachment.transition(true);
            }
            _ => unreachable!(),
        }
        let fresh = app
            .automation_session_mut(tab)
            .controller
            .capture_automation_input_fence();
        assert_eq!(fresh.is_some(), !matches!(transition, 1 | 3 | 5));
        assert_eq!(
            retry_automation_busy(|| coordinator.approve_control_handoff(1, &review, &decision)),
            Err(Error::Revoked)
        );
        assert!(coordinator.control_handoffs.is_empty());
        assert!(coordinator.pending_control_reviews.is_empty());
        let broker = coordinator.broker.as_ref().unwrap();
        assert!(!broker.pending_approval_is_active(principal, approval_id, Instant::now()));
        assert_eq!(
            broker.native_control_grant(principal, target, Instant::now()),
            Err(Error::Revoked)
        );

        if transition == 1 {
            ime.transition(true);
        }
        if transition == 3 {
            app.automation_session_mut(tab)
                .controller
                .cancel_clipboard_input(77, "test-cancel");
        }
        drop(intent);
        assert!(app
            .automation_session_mut(tab)
            .controller
            .capture_automation_input_fence()
            .is_some());
        assert_eq!(
            retry_automation_busy(|| coordinator.approve_control_handoff(1, &review, &decision)),
            Err(Error::Revoked)
        );
    }
}

#[test]
fn presentation_control_review_rejects_typing_after_prior_handoff_pruned() {
    use festerm_automation::Error;

    let _serial = presentation_admission_test_guard();
    let mut fixture = presented_input_fixture();
    let principal = fixture.coordinator.peers[&1].principal;
    fixture
        .coordinator
        .broker
        .as_mut()
        .unwrap()
        .revoke(principal);
    let (request, _, _) = presentation_control_request(fixture.barrier.target);
    let review = prepare_control_review(
        &mut fixture.coordinator,
        1,
        fixture.barrier.target,
        &request,
        &mut fixture.app.automation_session_mut(fixture.tab).controller,
    );
    let decision = review.accept_from_native();
    assert!(fixture.coordinator.control_handoffs.is_empty());
    type_in_presentation_tab(&mut fixture.app, fixture.tab);
    assert!(fixture
        .app
        .automation_session_mut(fixture.tab)
        .controller
        .capture_automation_input_fence()
        .is_some());
    assert_eq!(
        retry_automation_busy(|| fixture
            .coordinator
            .approve_control_handoff(1, &review, &decision)),
        Err(Error::Revoked)
    );
    assert!(fixture.coordinator.pending_control_reviews.is_empty());
    assert_eq!(
        fixture
            .coordinator
            .broker
            .as_ref()
            .unwrap()
            .native_control_grant(principal, fixture.barrier.target, Instant::now()),
        Err(Error::Revoked)
    );
}

#[test]
fn presentation_control_review_first_fresh_decision_succeeds_without_prior_handoff() {
    use festerm_automation::Error;

    let _serial = presentation_admission_test_guard();
    let (mut coordinator, mut app, tab, _ime, target) = control_review_fixture();
    let principal = coordinator.peers[&1].principal;
    let (request, _, _) = presentation_control_request(target);
    let stale = prepare_control_review(
        &mut coordinator,
        1,
        target,
        &request,
        &mut app.automation_session_mut(tab).controller,
    );
    let old_decision = stale.accept_from_native();
    type_in_presentation_tab(&mut app, tab);
    assert!(app
        .automation_session_mut(tab)
        .controller
        .capture_automation_input_fence()
        .is_some());
    let fresh = prepare_control_review(
        &mut coordinator,
        1,
        target,
        &request,
        &mut app.automation_session_mut(tab).controller,
    );
    assert_eq!(
        retry_automation_busy(|| coordinator.approve_control_handoff(1, &stale, &old_decision)),
        Err(Error::Revoked)
    );
    assert_eq!(
        retry_automation_busy(|| coordinator.approve_control_handoff(1, &fresh, &old_decision)),
        Err(Error::Unauthorized)
    );
    let fresh_decision = fresh.accept_from_native();
    retry_automation_busy(|| coordinator.approve_control_handoff(1, &fresh, &fresh_decision))
        .unwrap();
    assert!(coordinator
        .broker
        .as_ref()
        .unwrap()
        .native_control_grant(principal, target, Instant::now())
        .is_ok());
}

#[test]
fn presentation_control_review_is_channel_bound_and_disconnect_is_isolated() {
    use festerm_automation::Error;

    let _serial = presentation_admission_test_guard();
    let (mut coordinator, mut app, tab, _ime, target) = control_review_fixture();
    let principal_two = coordinator
        .broker
        .as_mut()
        .unwrap()
        .accept_connection()
        .unwrap();
    let (connection, _receive) = ipc::test_connection(2);
    coordinator.peers.insert(
        2,
        Peer {
            connection,
            principal: principal_two,
            machine: ready(),
            code: VerificationCode::new("0123456789abcdef").unwrap(),
            grant: None,
            waits: Vec::new(),
            controller: controller::PeerState::default(),
        },
    );
    let (request, _, _) = presentation_control_request(target);
    let first = prepare_control_review(
        &mut coordinator,
        1,
        target,
        &request,
        &mut app.automation_session_mut(tab).controller,
    );
    let second = prepare_control_review(
        &mut coordinator,
        2,
        target,
        &request,
        &mut app.automation_session_mut(tab).controller,
    );
    assert_eq!(
        approve_control_review(&mut coordinator, 2, &first),
        Err(Error::Unauthorized)
    );
    coordinator.peers[&1].connection.close();
    assert_eq!(
        approve_control_review(&mut coordinator, 1, &first),
        Err(Error::Revoked)
    );
    approve_control_review(&mut coordinator, 2, &second).unwrap();
    assert!(coordinator
        .broker
        .as_ref()
        .unwrap()
        .native_control_grant(principal_two, target, Instant::now())
        .is_ok());
}

#[test]
fn presentation_control_review_refuses_a_different_controller_and_privacy_loss() {
    use festerm_automation::Error;

    let _serial = presentation_admission_test_guard();
    let (mut coordinator, mut app, tab, _ime, target) = control_review_fixture();
    let (_other_coordinator, mut other_app, other_tab, _other_ime, _) = control_review_fixture();
    let (request, _, _) = presentation_control_request(target);
    assert!(matches!(
        retry_automation_busy(|| coordinator.prepare_control_handoff_review(
            1,
            target,
            request.clone(),
            Duration::from_secs(60),
            &mut other_app.automation_session_mut(other_tab).controller,
        )),
        Err(Error::BindingMismatch)
    ));
    assert!(coordinator.pending_control_reviews.is_empty());

    let review = prepare_control_review(
        &mut coordinator,
        1,
        target,
        &request,
        &mut app.automation_session_mut(tab).controller,
    );
    let decision = review.accept_from_native();
    let principal = coordinator.peers[&1].principal;
    let approval_id = *coordinator.pending_control_reviews.keys().next().unwrap();
    coordinator.privacy_gate.close();
    assert_eq!(
        retry_automation_busy(|| coordinator.approve_control_handoff(1, &review, &decision)),
        Err(Error::Revoked)
    );
    assert!(coordinator.pending_control_reviews.is_empty());
    assert!(!coordinator
        .broker
        .as_ref()
        .unwrap()
        .pending_approval_is_active(principal, approval_id, Instant::now()));
}

#[test]
fn presentation_control_review_freezes_the_displayed_source_closure_and_lifetime() {
    use festerm_automation::Error;

    let _serial = presentation_admission_test_guard();
    for observe in [false, true] {
        let (mut coordinator, mut app, tab, _ime, target) = control_review_fixture();
        let principal = coordinator.peers[&1].principal;
        let (mut request, _, binding) = presentation_control_request(target);
        if observe {
            request.scope.observe.insert(target);
        } else {
            request.scope.discover.insert(target);
        }
        for lifetime in [
            Duration::ZERO,
            festerm_automation::MAX_GRANT_LIFETIME + Duration::from_secs(1),
        ] {
            assert!(matches!(
                retry_automation_busy(|| coordinator.prepare_control_handoff_review(
                    1,
                    target,
                    request.clone(),
                    lifetime,
                    &mut app.automation_session_mut(tab).controller,
                )),
                Err(Error::InvalidRequest)
            ));
        }
        assert!(coordinator.pending_control_reviews.is_empty());
        let review = prepare_control_review(
            &mut coordinator,
            1,
            target,
            &request,
            &mut app.automation_session_mut(tab).controller,
        );
        approve_control_review(&mut coordinator, 1, &review).unwrap();
        let now = Instant::now();
        coordinator
            .broker
            .as_mut()
            .unwrap()
            .admit_run(
                principal,
                None,
                RunSpec {
                    binding,
                    scope: request.scope,
                    budget: Budget::default(),
                    deadline: now + Duration::from_secs(30),
                },
                now,
            )
            .unwrap();
    }
}

#[test]
fn presentation_control_review_rejects_stale_sources_before_native_decision() {
    use festerm_automation::{Error, RunState};

    let _serial = presentation_admission_test_guard();
    for discover in [false, true] {
        for change in 0..6 {
            let (mut source_app, source_tab) = FesTermApp::automation_fixture();
            let (mut coordinator, mut app, tab, _ime, target) =
                control_review_fixture_with_sources(&[&source_app]);
            let source = coordinator.targets[&source_tab.chip_id()].reference;
            let principal = coordinator.peers[&1].principal;
            let (observer, observer_run, _) = independent_run(&mut coordinator, target);
            let (mut request, _, _) = presentation_control_request(target);
            if discover {
                request.scope.discover.insert(source);
            } else {
                request.scope.observe.insert(source);
            }
            let review = prepare_control_review(
                &mut coordinator,
                1,
                target,
                &request,
                &mut app.automation_session_mut(tab).controller,
            );
            let decision = review.accept_from_native();
            let approval = *coordinator.pending_control_reviews.keys().next().unwrap();
            assert!(coordinator.control_handoffs.is_empty());

            change_readable_source(&mut source_app, source_tab, &mut coordinator, change);
            assert_eq!(coordinator.targets[&source_tab.chip_id()].reference, source);
            assert!(app
                .automation_session_mut(tab)
                .controller
                .capture_automation_input_fence()
                .is_some());
            assert_eq!(
                retry_automation_busy(|| {
                    coordinator.approve_control_handoff(1, &review, &decision)
                }),
                Err(Error::Revoked)
            );
            assert!(coordinator.pending_control_reviews.is_empty());
            let broker = coordinator.broker.as_mut().unwrap();
            assert!(!broker.pending_approval_is_active(principal, approval, Instant::now()));
            assert_eq!(
                broker.native_control_grant(principal, target, Instant::now()),
                Err(Error::Revoked)
            );
            assert_eq!(
                broker
                    .status(observer, observer_run, Instant::now())
                    .unwrap()
                    .state,
                RunState::Running
            );
            assert!(matches!(
                retry_automation_busy(|| coordinator.prepare_control_handoff_review(
                    1,
                    target,
                    request.clone(),
                    Duration::from_secs(60),
                    &mut app.automation_session_mut(tab).controller,
                )),
                Err(Error::StaleTarget)
            ));
        }
    }
}

#[test]
fn presentation_control_review_refuses_stale_source_before_prompt_creation() {
    use festerm_automation::Error;

    let _serial = presentation_admission_test_guard();
    for change in 0..5 {
        let (mut source_app, source_tab) = FesTermApp::automation_fixture();
        let (mut coordinator, mut app, tab, _ime, target) =
            control_review_fixture_with_sources(&[&source_app]);
        let source = coordinator.targets[&source_tab.chip_id()].reference;
        let (mut request, _, _) = presentation_control_request(target);
        request.scope.observe.insert(source);
        change_readable_source(&mut source_app, source_tab, &mut coordinator, change);
        assert!(matches!(
            retry_automation_busy(|| coordinator.prepare_control_handoff_review(
                1,
                target,
                request.clone(),
                Duration::from_secs(60),
                &mut app.automation_session_mut(tab).controller,
            )),
            Err(Error::StaleTarget)
        ));
        assert!(coordinator.pending_control_reviews.is_empty());
        let (unrelated, _, _) = presentation_control_request(target);
        let review = prepare_control_review(
            &mut coordinator,
            1,
            target,
            &unrelated,
            &mut app.automation_session_mut(tab).controller,
        );
        approve_control_review(&mut coordinator, 1, &review).unwrap();
    }
}

#[test]
fn presentation_stale_pending_source_preserves_unrelated_existing_handoff() {
    use festerm_automation::{Error, RunState};

    let _serial = presentation_admission_test_guard();
    let (source_app, source_tab) = FesTermApp::automation_fixture();
    let (mut coordinator, mut app, tab, _ime, target) =
        control_review_fixture_with_sources(&[&source_app]);
    let source = coordinator.targets[&source_tab.chip_id()].reference;
    let principal = coordinator.peers[&1].principal;
    let (request, scope, binding) = presentation_control_request(target);
    let initial = prepare_control_review(
        &mut coordinator,
        1,
        target,
        &request,
        &mut app.automation_session_mut(tab).controller,
    );
    approve_control_review(&mut coordinator, 1, &initial).unwrap();
    let now = Instant::now();
    let broker = coordinator.broker.as_mut().unwrap();
    let grant = broker.native_control_grant(principal, target, now).unwrap();
    let run = broker
        .admit_run(
            principal,
            None,
            RunSpec {
                binding,
                scope,
                budget: Budget::default(),
                deadline: now + Duration::from_secs(30),
            },
            now,
        )
        .unwrap();
    let lease =
        retry_automation_busy(|| coordinator.acquire_control_lease(1, run, target)).unwrap();
    let mut wider = request;
    wider.scope.observe.insert(source);
    let review = prepare_control_review(
        &mut coordinator,
        1,
        target,
        &wider,
        &mut app.automation_session_mut(tab).controller,
    );
    let decision = review.accept_from_native();
    disconnect_source_without_reconciliation(&source_app, source_tab);
    assert_eq!(
        retry_automation_busy(|| coordinator.approve_control_handoff(1, &review, &decision)),
        Err(Error::Revoked)
    );
    let broker = coordinator.broker.as_mut().unwrap();
    assert_eq!(
        broker.native_control_grant(principal, target, Instant::now()),
        Ok(grant)
    );
    assert_eq!(
        broker.status(principal, run, Instant::now()).unwrap().state,
        RunState::Running
    );
    assert!(broker
        .prepare_request(
            principal,
            run,
            AuthorityOperation::Control {
                lease,
                input: Digest(Sha256::digest(b"test").into()),
                bytes: 4,
                presentation: None,
            },
            Instant::now(),
        )
        .is_ok());
}

#[test]
fn presentation_lease_refuses_source_attachment_loss_and_regain() {
    use festerm_automation::{Error, RunState};

    let _serial = presentation_admission_test_guard();
    let (source_app, source_tab) = FesTermApp::automation_fixture();
    let (mut coordinator, mut app, tab, _ime, target) =
        control_review_fixture_with_sources(&[&source_app]);
    let source = coordinator.targets[&source_tab.chip_id()].reference;
    let principal = coordinator.peers[&1].principal;
    let (observer, observer_run, _) = independent_run(&mut coordinator, target);
    let (mut request, scope, binding) = presentation_control_request(target);
    request.scope.observe.insert(source);
    let review = prepare_control_review(
        &mut coordinator,
        1,
        target,
        &request,
        &mut app.automation_session_mut(tab).controller,
    );
    approve_control_review(&mut coordinator, 1, &review).unwrap();
    let now = Instant::now();
    let run = coordinator
        .broker
        .as_mut()
        .unwrap()
        .admit_run(
            principal,
            None,
            RunSpec {
                binding,
                scope,
                budget: Budget::default(),
                deadline: now + Duration::from_secs(30),
            },
            now,
        )
        .unwrap();
    replace_source_attachment_without_event(&source_app, source_tab);
    assert_eq!(
        retry_automation_busy(|| coordinator.acquire_control_lease(1, run, target)),
        Err(Error::Revoked)
    );
    let broker = coordinator.broker.as_mut().unwrap();
    assert_eq!(
        broker.native_control_grant(principal, target, Instant::now()),
        Err(Error::Revoked)
    );
    assert_eq!(
        broker
            .status(observer, observer_run, Instant::now())
            .unwrap()
            .state,
        RunState::Running
    );
}

#[test]
fn presentation_first_fresh_review_succeeds_after_source_cut() {
    use festerm_automation::RunState;
    use festerm_session::AutomationInputStatus;

    let _serial = presentation_admission_test_guard();
    let (source_app, source_tab) = FesTermApp::automation_fixture();
    let mut fixture = presented_input_fixture_with_sources(&[(&source_app, source_tab, false)]);
    let target = fixture.barrier.target;
    let (observer, observer_run, _) = independent_run(&mut fixture.coordinator, target);
    let (reservation, mut pending) = reserve_presented_input(&fixture.coordinator, target, b"test");
    replace_source_attachment_without_event(&source_app, source_tab);
    let (request, scope, binding) = presentation_control_request(target);
    let review = prepare_control_review(
        &mut fixture.coordinator,
        1,
        target,
        &request,
        &mut fixture.app.automation_session_mut(fixture.tab).controller,
    );
    assert!(fixture
        .coordinator
        .commit_presented_input(1, fixture.request, reservation, &mut fixture.barrier)
        .is_err());
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
    approve_control_review(&mut fixture.coordinator, 1, &review).unwrap();
    let principal = fixture.coordinator.peers[&1].principal;
    let now = Instant::now();
    let broker = fixture.coordinator.broker.as_mut().unwrap();
    let run = broker
        .admit_run(
            principal,
            None,
            RunSpec {
                binding,
                scope,
                budget: Budget::default(),
                deadline: now + Duration::from_secs(30),
            },
            now,
        )
        .unwrap();
    assert_eq!(
        broker.status(observer, observer_run, now).unwrap().state,
        RunState::Running
    );
    retry_automation_busy(|| fixture.coordinator.acquire_control_lease(1, run, target)).unwrap();
}

#[test]
fn presentation_control_review_slots_are_reclaimed_after_cancel_revocation_and_expiry() {
    use festerm_automation::Error;

    let _serial = presentation_admission_test_guard();
    let (mut coordinator, mut app, tab, _ime, target) = control_review_fixture();
    let principal = coordinator.peers[&1].principal;
    let (request, _, _) = presentation_control_request(target);
    let first = prepare_control_review(
        &mut coordinator,
        1,
        target,
        &request,
        &mut app.automation_session_mut(tab).controller,
    );
    let second = prepare_control_review(
        &mut coordinator,
        1,
        target,
        &request,
        &mut app.automation_session_mut(tab).controller,
    );
    assert!(matches!(
        retry_automation_busy(|| coordinator.prepare_control_handoff_review(
            1,
            target,
            request.clone(),
            Duration::from_secs(60),
            &mut app.automation_session_mut(tab).controller,
        )),
        Err(Error::QuotaExceeded)
    ));
    retry_automation_busy(|| coordinator.cancel_control_handoff_review(1, &first)).unwrap();
    assert_eq!(
        approve_control_review(&mut coordinator, 1, &first),
        Err(Error::Revoked)
    );
    let third = prepare_control_review(
        &mut coordinator,
        1,
        target,
        &request,
        &mut app.automation_session_mut(tab).controller,
    );
    coordinator.broker.as_mut().unwrap().revoke(principal);
    let fresh = prepare_control_review(
        &mut coordinator,
        1,
        target,
        &request,
        &mut app.automation_session_mut(tab).controller,
    );
    assert_eq!(
        approve_control_review(&mut coordinator, 1, &second),
        Err(Error::Revoked)
    );
    assert_eq!(
        approve_control_review(&mut coordinator, 1, &third),
        Err(Error::Revoked)
    );
    coordinator.broker.as_mut().unwrap().poll_expiry(
        Instant::now() + festerm_automation::MAX_GRANT_LIFETIME + Duration::from_secs(1),
    );
    let after_expiry = prepare_control_review(
        &mut coordinator,
        1,
        target,
        &request,
        &mut app.automation_session_mut(tab).controller,
    );
    assert_eq!(
        approve_control_review(&mut coordinator, 1, &fresh),
        Err(Error::Revoked)
    );
    approve_control_review(&mut coordinator, 1, &after_expiry).unwrap();
    assert!(coordinator.pending_control_reviews.is_empty());
}

#[test]
fn presentation_input_admission_cuts_old_handoff_after_typing_and_fresh_capture() {
    use festerm_automation::Error;
    use festerm_session::{AutomationInputStatus, CommitAutomationInputError};

    let _serial = presentation_admission_test_guard();
    let PresentedInputFixture {
        mut coordinator,
        mut app,
        tab,
        run,
        lease,
        request,
        mut barrier,
        ..
    } = presented_input_fixture();
    type_in_presentation_tab(&mut app, tab);
    let fresh = app
        .automation_session_mut(tab)
        .controller
        .capture_automation_input_fence()
        .expect("a new capture is possible after the human write drains");
    {
        let _guard = festerm_session::automation_admission_lock().unwrap();
        assert!(fresh.is_current());
    }

    let (result, mut pending) =
        commit_presented_input_retry(&mut coordinator, 1, request, &mut barrier, b"test");
    assert!(matches!(
        result,
        Err(CommitAutomationInputError::Refused(
            Error::Closed | Error::Revoked
        ))
    ));
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
    let principal = coordinator.peers[&1].principal;
    let broker = coordinator.broker.as_mut().unwrap();
    assert_eq!(
        broker.native_control_grant(principal, lease.target(), Instant::now()),
        Err(Error::Revoked)
    );
    assert!(broker
        .prepare_lease(principal, run, lease.target(), Instant::now())
        .is_err());
    assert!(
        retry_automation_busy(|| coordinator.acquire_control_lease(1, run, barrier.target))
            .is_err()
    );
}

#[test]
fn presentation_input_admission_requires_new_native_decision_after_takeover() {
    use festerm_automation::Error;

    let _serial = presentation_admission_test_guard();
    let PresentedInputFixture {
        mut coordinator,
        mut app,
        tab,
        run,
        lease,
        request: old_request,
        barrier,
        ..
    } = presented_input_fixture();
    let principal = coordinator.peers[&1].principal;
    let (grant, scope, binding) = presentation_control_request(barrier.target);
    let stale_review = prepare_control_review(
        &mut coordinator,
        1,
        barrier.target,
        &grant,
        &mut app.automation_session_mut(tab).controller,
    );
    let stale_decision = stale_review.accept_from_native();
    type_in_presentation_tab(&mut app, tab);
    assert!(app
        .automation_session_mut(tab)
        .controller
        .capture_automation_input_fence()
        .is_some());

    // The first new proposal must snapshot ownership after the takeover cut.
    let fresh_review = prepare_control_review(
        &mut coordinator,
        1,
        barrier.target,
        &grant,
        &mut app.automation_session_mut(tab).controller,
    );
    assert_eq!(
        retry_automation_busy(|| {
            coordinator.approve_control_handoff(1, &fresh_review, &stale_decision)
        }),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        retry_automation_busy(|| {
            coordinator.approve_control_handoff(1, &stale_review, &stale_decision)
        }),
        Err(Error::Revoked)
    );
    let broker = coordinator.broker.as_mut().unwrap();
    assert_eq!(
        broker.native_control_grant(principal, barrier.target, Instant::now()),
        Err(Error::Revoked)
    );
    assert!(broker
        .prepared_control_target(principal, old_request, Instant::now())
        .is_err());
    assert!(broker
        .prepare_lease(principal, run, barrier.target, Instant::now())
        .is_err());

    approve_control_review(&mut coordinator, 1, &fresh_review).unwrap();
    let now = Instant::now();
    let new_run = coordinator
        .broker
        .as_mut()
        .unwrap()
        .admit_run(
            principal,
            None,
            RunSpec {
                binding,
                scope,
                budget: Budget::default(),
                deadline: now + Duration::from_secs(30),
            },
            now,
        )
        .unwrap();
    let new_lease =
        retry_automation_busy(|| coordinator.acquire_control_lease(1, new_run, barrier.target))
            .unwrap();
    assert_ne!(new_lease, lease);
    assert!(coordinator
        .broker
        .as_ref()
        .unwrap()
        .prepared_control_target(principal, old_request, Instant::now())
        .is_err());
}

#[test]
fn presentation_input_admission_cuts_ime_loss_regain_and_pending_human_input() {
    use festerm_automation::Error;
    use festerm_session::AutomationInputStatus;

    let _serial = presentation_admission_test_guard();
    for regain_before_admission in [false, true] {
        let PresentedInputFixture {
            mut coordinator,
            mut app,
            tab,
            ime,
            run,
            request,
            mut barrier,
            ..
        } = presented_input_fixture();
        ime.transition(false);
        assert!(app
            .automation_session_mut(tab)
            .controller
            .capture_automation_input_fence()
            .is_none());
        if regain_before_admission {
            ime.transition(true);
            assert!(app
                .automation_session_mut(tab)
                .controller
                .capture_automation_input_fence()
                .is_some());
        }
        let (result, mut pending) =
            commit_presented_input_retry(&mut coordinator, 1, request, &mut barrier, b"test");
        assert!(result.is_err());
        assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
        if !regain_before_admission {
            ime.transition(true);
            assert!(app
                .automation_session_mut(tab)
                .controller
                .capture_automation_input_fence()
                .is_some());
        }
        let principal = coordinator.peers[&1].principal;
        assert_eq!(
            coordinator.broker.as_ref().unwrap().native_control_grant(
                principal,
                barrier.target,
                Instant::now()
            ),
            Err(Error::Revoked)
        );
        assert!(retry_automation_busy(|| coordinator.acquire_control_lease(
            1,
            run,
            barrier.target
        ))
        .is_err());
    }

    let PresentedInputFixture {
        mut coordinator,
        mut app,
        tab,
        run,
        request,
        mut barrier,
        ..
    } = presented_input_fixture();
    let controller = &mut app.automation_session_mut(tab).controller;
    assert!(controller.begin_clipboard_input(77));
    assert!(controller.capture_automation_input_fence().is_none());
    let (result, mut pending) =
        commit_presented_input_retry(&mut coordinator, 1, request, &mut barrier, b"test");
    assert!(result.is_err());
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
    controller.cancel_clipboard_input(77, "test-cancel");
    assert!(controller.capture_automation_input_fence().is_some());
    assert!(
        retry_automation_busy(|| coordinator.acquire_control_lease(1, run, barrier.target))
            .is_err()
    );
}

#[test]
fn presentation_input_admission_refuses_pending_ime_at_native_handoff() {
    use festerm_automation::Error;

    let _serial = presentation_admission_test_guard();
    let PresentedInputFixture {
        mut coordinator,
        mut app,
        tab,
        ime,
        barrier,
        ..
    } = presented_input_fixture();
    let (grant, _, _) = presentation_control_request(barrier.target);
    let review = prepare_control_review(
        &mut coordinator,
        1,
        barrier.target,
        &grant,
        &mut app.automation_session_mut(tab).controller,
    );
    ime.transition(false);
    assert_eq!(
        approve_control_review(&mut coordinator, 1, &review),
        Err(Error::Revoked)
    );
    assert!(coordinator.control_handoffs.is_empty());
    assert!(matches!(
        retry_automation_busy(|| coordinator.prepare_control_handoff_review(
            1,
            barrier.target,
            grant.clone(),
            Duration::from_secs(60),
            &mut app.automation_session_mut(tab).controller,
        )),
        Err(Error::Unavailable)
    ));
    ime.transition(true);
    assert_eq!(
        approve_control_review(&mut coordinator, 1, &review),
        Err(Error::Revoked)
    );
}

#[test]
fn presentation_input_admission_refuses_other_backend_and_controller_reservations() {
    use festerm_automation::Error;
    use festerm_session::{AutomationInputStatus, CommitAutomationInputError};

    let _serial = presentation_admission_test_guard();
    let mut a = presented_input_fixture();
    let b = presented_input_fixture();
    let target_a = &a.coordinator.targets[&a.barrier.target.session()];
    let target_b = &b.coordinator.targets[&b.barrier.target.session()];
    assert_eq!(
        target_a.attachment_generation,
        target_b.attachment_generation
    );
    let owner_a = target_a.controller_instance;
    let owner_b = target_b.controller_instance;
    let attachment_a = target_a.attachment.clone();
    let attachment_b = target_b.attachment.clone();
    assert_ne!(owner_a, owner_b);
    assert!(!Arc::ptr_eq(&attachment_a, &attachment_b));

    for (owner, attachment) in [
        (owner_b, attachment_b.clone()),
        (owner_a, attachment_b),
        (owner_b, attachment_a),
    ] {
        let (result, mut pending) = commit_presented_input_with_retry(
            &mut a.coordinator,
            1,
            a.request,
            &mut a.barrier,
            |_| reserve_for_binding(owner, attachment.clone(), b"test"),
        );
        assert_eq!(
            result,
            Err(CommitAutomationInputError::Refused(Error::BindingMismatch))
        );
        assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
    }
    let (result, mut pending) =
        commit_presented_input_retry(&mut a.coordinator, 1, a.request, &mut a.barrier, b"test");
    result.unwrap();
    assert!(matches!(pending.poll(), AutomationInputStatus::Ready(_)));
}

#[test]
fn presentation_input_admission_refuses_reservation_from_replaced_attachment() {
    use festerm_automation::Error;
    use festerm_session::{AutomationInputStatus, CommitAutomationInputError};

    let _serial = presentation_admission_test_guard();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let PresentedInputFixture {
            mut coordinator,
            mut app,
            tab,
            run,
            request,
            mut barrier,
            ..
        } = presented_input_fixture();
        let (reservation, mut pending) =
            reserve_presented_input(&coordinator, barrier.target, b"test");
        let controller = &mut app.automation_session_mut(tab).controller;
        let attachment = controller.automation_attachment_epoch().unwrap();
        attachment.transition(false);
        attachment.transition(true);
        assert!(controller.capture_automation_input_fence().is_some());
        let result = coordinator.commit_presented_input(1, request, reservation, &mut barrier);
        assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
        if matches!(
            result,
            Err(CommitAutomationInputError::Refused(Error::Busy))
        ) && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        assert!(matches!(
            result,
            Err(CommitAutomationInputError::Refused(
                Error::Closed | Error::Revoked
            ))
        ));
        let principal = coordinator.peers[&1].principal;
        assert_eq!(
            coordinator.broker.as_ref().unwrap().native_control_grant(
                principal,
                barrier.target,
                Instant::now()
            ),
            Err(Error::Revoked)
        );
        assert!(retry_automation_busy(|| coordinator.acquire_control_lease(
            1,
            run,
            barrier.target
        ))
        .is_err());
        break;
    }
}

#[test]
fn presentation_input_admission_requires_the_bound_run_and_lease() {
    use festerm_automation::Error;
    use festerm_session::{AutomationInputStatus, CommitAutomationInputError};

    let _serial = presentation_admission_test_guard();
    let PresentedInputFixture {
        mut coordinator,
        app: _app,
        run,
        lease,
        requirement,
        mut barrier,
        ..
    } = presented_input_fixture();
    let principal = coordinator.peers[&1].principal;
    let (_, scope, binding) = presentation_control_request(barrier.target);
    let now = Instant::now();
    let broker = coordinator.broker.as_mut().unwrap();
    broker.cancel_run(principal, run, now).unwrap();
    let other_run = broker
        .admit_run(
            principal,
            None,
            RunSpec {
                binding,
                scope,
                budget: Budget::default(),
                deadline: now + Duration::from_secs(30),
            },
            now,
        )
        .unwrap();
    let transition = broker
        .prepare_lease(principal, other_run, barrier.target, now)
        .unwrap();
    let unbound_lease = broker.commit_lease(transition, now).unwrap();
    assert_ne!(lease, unbound_lease);
    let request = broker
        .prepare_request(
            principal,
            other_run,
            AuthorityOperation::Control {
                lease: unbound_lease,
                input: Digest(Sha256::digest(b"test").into()),
                bytes: 4,
                presentation: Some(requirement),
            },
            now,
        )
        .unwrap();
    let (result, mut pending) =
        commit_presented_input_retry(&mut coordinator, 1, request, &mut barrier, b"test");
    assert_eq!(
        result,
        Err(CommitAutomationInputError::Refused(Error::BindingMismatch))
    );
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
}

#[test]
fn presentation_input_admission_barrier_failure_after_publication_is_unknown() {
    use festerm_automation::Error;
    use festerm_session::{AutomationInputStatus, CommitAutomationInputError};

    let _serial = presentation_admission_test_guard();
    let mut fixture = presented_input_fixture();
    fixture.barrier.fail_after_operation = true;
    let principal = fixture.coordinator.peers[&1].principal;
    let second_request = fixture
        .coordinator
        .broker
        .as_mut()
        .unwrap()
        .prepare_request(
            principal,
            fixture.run,
            AuthorityOperation::Control {
                lease: fixture.lease,
                input: Digest(Sha256::digest(b"test").into()),
                bytes: 4,
                presentation: Some(fixture.requirement),
            },
            Instant::now(),
        )
        .unwrap();
    let (result, mut pending) = commit_presented_input_retry(
        &mut fixture.coordinator,
        1,
        fixture.request,
        &mut fixture.barrier,
        b"test",
    );
    assert_eq!(result, Err(CommitAutomationInputError::LostAfterCommit));
    assert!(matches!(
        pending.poll(),
        AutomationInputStatus::Ready(bytes) if &*bytes == b"test"
    ));
    let status = fixture
        .coordinator
        .broker
        .as_mut()
        .unwrap()
        .status(principal, fixture.run, Instant::now())
        .unwrap();
    assert_eq!(status.admitted_effects, 1);
    assert!(status.execution_outcome_unknown);
    let (result, mut replay_pending) = commit_presented_input_retry(
        &mut fixture.coordinator,
        1,
        fixture.request,
        &mut fixture.barrier,
        b"test",
    );
    assert!(result.is_err());
    assert!(matches!(
        replay_pending.poll(),
        AutomationInputStatus::Cancelled
    ));
    let (result, mut second_pending) = commit_presented_input_retry(
        &mut fixture.coordinator,
        1,
        second_request,
        &mut fixture.barrier,
        b"test",
    );
    assert!(result.is_err());
    assert!(matches!(
        second_pending.poll(),
        AutomationInputStatus::Cancelled
    ));

    let mut refused = presented_input_fixture();
    refused.barrier.fail_after_operation = true;
    let (result, mut pending) = commit_presented_input_retry(
        &mut refused.coordinator,
        1,
        refused.request,
        &mut refused.barrier,
        b"wrong",
    );
    assert_eq!(
        result,
        Err(CommitAutomationInputError::Refused(Error::BindingMismatch))
    );
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
}

#[test]
fn presentation_input_admission_is_nonblocking_under_admission_contention() {
    use festerm_automation::Error;
    use festerm_session::{AutomationInputStatus, CommitAutomationInputError};

    let _serial = presentation_admission_test_guard();
    let mut fixture = presented_input_fixture();
    let (grant, _, _) = presentation_control_request(fixture.barrier.target);
    let review = prepare_control_review(
        &mut fixture.coordinator,
        1,
        fixture.barrier.target,
        &grant,
        &mut fixture.app.automation_session_mut(fixture.tab).controller,
    );
    let decision = review.accept_from_native();
    let (reservation, mut pending) =
        reserve_presented_input(&fixture.coordinator, fixture.barrier.target, b"test");
    let (held_send, held) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let holder = std::thread::spawn(move || {
        let _guard = festerm_session::automation_admission_lock().unwrap();
        held_send.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(2)).is_ok()
    });
    held.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(
        fixture.coordinator.commit_presented_input(
            1,
            fixture.request,
            reservation,
            &mut fixture.barrier,
        ),
        Err(CommitAutomationInputError::Refused(Error::Busy))
    );
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
    assert_eq!(
        fixture
            .coordinator
            .acquire_control_lease(1, fixture.run, fixture.barrier.target),
        Err(Error::Busy)
    );
    assert_eq!(
        fixture
            .coordinator
            .approve_control_handoff(1, &review, &decision),
        Err(Error::Busy)
    );
    assert!(matches!(
        fixture.coordinator.prepare_control_handoff_review(
            1,
            fixture.barrier.target,
            grant,
            Duration::from_secs(60),
            &mut fixture.app.automation_session_mut(fixture.tab).controller,
        ),
        Err(Error::Busy)
    ));
    release.send(()).unwrap();
    assert!(holder.join().unwrap());
    let (result, mut pending) = commit_presented_input_retry(
        &mut fixture.coordinator,
        1,
        fixture.request,
        &mut fixture.barrier,
        b"test",
    );
    result.unwrap();
    assert!(matches!(pending.poll(), AutomationInputStatus::Ready(_)));
    retry_automation_busy(|| {
        fixture
            .coordinator
            .approve_control_handoff(1, &review, &decision)
    })
    .unwrap();
}

#[test]
fn presentation_input_admission_is_principal_bound_and_takeover_cuts_all_controllers() {
    use festerm_automation::{Error, RunState};
    use festerm_session::{AutomationInputStatus, CommitAutomationInputError};

    let _serial = presentation_admission_test_guard();
    let mut fixture = presented_input_fixture();
    let principal_one = fixture.coordinator.peers[&1].principal;
    let principal_two = fixture
        .coordinator
        .broker
        .as_mut()
        .unwrap()
        .accept_connection()
        .unwrap();
    let (connection, _receive) = ipc::test_connection(2);
    fixture.coordinator.peers.insert(
        2,
        Peer {
            connection,
            principal: principal_two,
            machine: ready(),
            code: VerificationCode::new("0123456789abcdef").unwrap(),
            grant: None,
            waits: Vec::new(),
            controller: controller::PeerState::default(),
        },
    );
    let now = Instant::now();
    let (grant, scope, binding) = presentation_control_request(fixture.barrier.target);
    let review = prepare_control_review(
        &mut fixture.coordinator,
        2,
        fixture.barrier.target,
        &grant,
        &mut fixture.app.automation_session_mut(fixture.tab).controller,
    );
    assert_eq!(
        approve_control_review(&mut fixture.coordinator, 1, &review),
        Err(Error::Unauthorized)
    );
    approve_control_review(&mut fixture.coordinator, 2, &review).unwrap();
    let second_run = fixture
        .coordinator
        .broker
        .as_mut()
        .unwrap()
        .admit_run(
            principal_two,
            None,
            RunSpec {
                binding,
                scope,
                budget: Budget::default(),
                deadline: now + Duration::from_secs(30),
            },
            now,
        )
        .unwrap();
    let (observer, observer_run, _) =
        independent_run(&mut fixture.coordinator, fixture.barrier.target);
    let (result, mut pending) = commit_presented_input_retry(
        &mut fixture.coordinator,
        2,
        fixture.request,
        &mut fixture.barrier,
        b"test",
    );
    assert_eq!(
        result,
        Err(CommitAutomationInputError::Refused(Error::Unauthorized))
    );
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));

    type_in_presentation_tab(&mut fixture.app, fixture.tab);
    assert!(fixture
        .app
        .automation_session_mut(fixture.tab)
        .controller
        .capture_automation_input_fence()
        .is_some());
    let (result, mut pending) = commit_presented_input_retry(
        &mut fixture.coordinator,
        1,
        fixture.request,
        &mut fixture.barrier,
        b"test",
    );
    assert!(result.is_err());
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
    let broker = fixture.coordinator.broker.as_mut().unwrap();
    for (principal, run) in [(principal_one, fixture.run), (principal_two, second_run)] {
        assert_eq!(
            broker.native_control_grant(principal, fixture.barrier.target, Instant::now()),
            Err(Error::Revoked)
        );
        assert!(broker
            .prepare_lease(principal, run, fixture.barrier.target, Instant::now())
            .is_err());
    }
    assert_eq!(
        broker
            .status(observer, observer_run, Instant::now())
            .unwrap()
            .state,
        RunState::Running
    );
}

#[test]
fn presentation_retired_handoff_does_not_revoke_unrelated_principal_on_fresh_review() {
    use festerm_automation::{Error, RunState};
    use festerm_session::AutomationInputStatus;

    let _serial = presentation_admission_test_guard();
    let (other_app, other_tab) = FesTermApp::automation_fixture();
    let mut fixture = presented_input_fixture_with_sources(&[(&other_app, other_tab, false)]);
    let principal = fixture.coordinator.peers[&1].principal;
    let old_target = fixture.barrier.target;
    let other_target = fixture.coordinator.targets[&other_tab.chip_id()].reference;
    let (observer, observer_run, _) = independent_run(&mut fixture.coordinator, other_target);
    let (reservation, mut pending) =
        reserve_presented_input(&fixture.coordinator, old_target, b"test");

    let old_instance = fixture
        .app
        .automation_session(fixture.tab)
        .unwrap()
        .controller
        .automation_instance();
    assert_eq!(
        fixture.app.automation_replace_fixture_session(),
        fixture.tab
    );
    assert_ne!(
        fixture
            .app
            .automation_session(fixture.tab)
            .unwrap()
            .controller
            .automation_instance(),
        old_instance
    );
    fixture.app.set_automation_ime_epoch(&fixture.ime);
    let mut broker = fixture.coordinator.broker.take().unwrap();
    fixture
        .coordinator
        .reconcile(&mut broker, &[&fixture.app, &other_app]);
    let replacement = fixture
        .coordinator
        .targets
        .get_mut(&fixture.tab.chip_id())
        .unwrap();
    replacement.local_pty = true;
    let replacement = replacement.reference;
    assert_ne!(replacement, old_target);
    assert_eq!(
        broker.native_control_grant(principal, old_target, Instant::now()),
        Err(Error::StaleTarget)
    );
    assert!(fixture
        .coordinator
        .control_handoffs
        .contains_key(&principal));
    fixture.coordinator.broker = Some(broker);

    let (request, scope, binding) = presentation_control_request(replacement);
    let review = prepare_control_review(
        &mut fixture.coordinator,
        1,
        replacement,
        &request,
        &mut fixture.app.automation_session_mut(fixture.tab).controller,
    );
    let result = fixture.coordinator.commit_presented_input(
        1,
        fixture.request,
        reservation,
        &mut fixture.barrier,
    );
    assert!(result.is_err());
    assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
    approve_control_review(&mut fixture.coordinator, 1, &review).unwrap();
    let now = Instant::now();
    let broker = fixture.coordinator.broker.as_mut().unwrap();
    let run = broker
        .admit_run(
            principal,
            None,
            RunSpec {
                binding,
                scope,
                budget: Budget::default(),
                deadline: now + Duration::from_secs(30),
            },
            now,
        )
        .unwrap();
    assert_eq!(
        broker.status(observer, observer_run, now).unwrap().state,
        RunState::Running
    );
    let observation = broker
        .prepare_request(
            observer,
            observer_run,
            AuthorityOperation::Observe {
                target: other_target,
                max_bytes: 32,
            },
            now,
        )
        .unwrap();
    broker
        .commit_disclosure(observer, observation, 16, now)
        .unwrap();
    assert_eq!(
        retry_automation_busy(|| {
            fixture
                .coordinator
                .acquire_control_lease(1, run, replacement)
        })
        .unwrap()
        .target(),
        replacement
    );
}

#[test]
fn presentation_retired_source_does_not_revoke_its_reconciled_successor() {
    use festerm_automation::RunState;

    let _serial = presentation_admission_test_guard();
    let (mut source_app, source_tab) = FesTermApp::automation_fixture();
    let (other_app, other_tab) = FesTermApp::automation_fixture();
    let mut fixture = presented_input_fixture_with_sources(&[
        (&source_app, source_tab, false),
        (&other_app, other_tab, true),
    ]);
    let target = fixture.barrier.target;
    let old_source = fixture.coordinator.targets[&source_tab.chip_id()].reference;
    let other_target = fixture.coordinator.targets[&other_tab.chip_id()].reference;
    let (observer, observer_run, _) = independent_run(&mut fixture.coordinator, other_target);

    assert_eq!(source_app.automation_replace_fixture_session(), source_tab);
    // The test fixture substitutes an SSH backend for the controlled local PTY.
    fixture
        .coordinator
        .targets
        .get_mut(&fixture.tab.chip_id())
        .unwrap()
        .local_pty = false;
    let mut broker = fixture.coordinator.broker.take().unwrap();
    fixture
        .coordinator
        .reconcile(&mut broker, &[&fixture.app, &source_app, &other_app]);
    let controlled = fixture
        .coordinator
        .targets
        .get_mut(&fixture.tab.chip_id())
        .unwrap();
    assert_eq!(controlled.reference, target);
    controlled.local_pty = true;
    let successor = fixture.coordinator.targets[&source_tab.chip_id()].reference;
    assert_ne!(successor, old_source);
    fixture.coordinator.broker = Some(broker);
    let (successor_observer, successor_run, _) =
        independent_run(&mut fixture.coordinator, successor);

    let (mut request, _, _) = presentation_control_request(target);
    request.scope.observe.insert(successor);
    let review = prepare_control_review(
        &mut fixture.coordinator,
        1,
        target,
        &request,
        &mut fixture.app.automation_session_mut(fixture.tab).controller,
    );
    approve_control_review(&mut fixture.coordinator, 1, &review).unwrap();
    let broker = fixture.coordinator.broker.as_mut().unwrap();
    for (principal, run) in [
        (observer, observer_run),
        (successor_observer, successor_run),
    ] {
        assert_eq!(
            broker.status(principal, run, Instant::now()).unwrap().state,
            RunState::Running
        );
    }
    let observation = broker
        .prepare_request(
            successor_observer,
            successor_run,
            AuthorityOperation::Observe {
                target: successor,
                max_bytes: 32,
            },
            Instant::now(),
        )
        .unwrap();
    broker
        .commit_disclosure(successor_observer, observation, 16, Instant::now())
        .unwrap();
}

pub(crate) fn preference_application_fixture(
    approved: bool,
) -> (
    Coordinator,
    FesTermApp,
    TargetRef,
    Arc<AtomicBool>,
    impl Fn() -> Option<Vec<u8>>,
) {
    let (mut coordinator, app, id, receive) = fixture();
    let target = coordinator.targets[&id.chip_id()].reference;
    if approved {
        coordinator.approve(1, vec![target]).unwrap();
    }
    let stop = coordinator.stop.clone();
    (coordinator, app, target, stop, receive)
}

fn independent_run(
    coordinator: &mut Coordinator,
    target: TargetRef,
) -> (ConnectionId, RunId, Instant) {
    let now = Instant::now();
    let scope = Scope {
        observe: BTreeSet::from([target]),
        return_results: true,
        ..Scope::default()
    };
    let request = GrantRequest {
        scope: scope.clone(),
        bindings: BTreeSet::new(),
        direct_read_calls: BTreeSet::from([DirectKind::Observation]),
    };
    let binding = direct_binding(
        &Operation::TerminalReadScreen {
            session_ref: String::new(),
            max_bytes: 1024,
        },
        Some(target),
    )
    .unwrap();
    let broker = coordinator.broker.as_mut().unwrap();
    let connection = broker.accept_connection().unwrap();
    let approval = broker
        .request_approval(connection, request.clone(), now)
        .unwrap();
    broker
        .approve(
            connection,
            approval,
            NativeApproval::new(request, Duration::from_secs(300)),
            now,
        )
        .unwrap();
    let run = broker
        .admit_run(
            connection,
            None,
            RunSpec {
                binding,
                scope,
                budget: Budget::default(),
                deadline: now + Duration::from_secs(60),
            },
            now,
        )
        .unwrap();
    (connection, run, now)
}

fn call(
    coordinator: &mut Coordinator,
    apps: &[&FesTermApp],
    tool: &str,
    args: Value,
) -> RequestKey {
    let peer = coordinator.peers.get_mut(&1).unwrap();
    let event = peer.machine.process_value(json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": tool, "arguments": args},
    }));
    let Event::Backend(request) = event else {
        panic!("expected backend request")
    };
    let key = request.key.clone();
    execute(
        coordinator.broker.as_mut().unwrap(),
        &coordinator.targets,
        peer,
        request,
        apps,
        Instant::now(),
    );
    key
}

fn response(receive: &impl Fn() -> Option<Vec<u8>>) -> Value {
    serde_json::from_slice(&receive().expect("expected reply")).unwrap()
}

#[test]
fn controller_operations_never_enter_observation_grant_path() {
    let (mut coordinator, mut app, id, receive) = fixture();
    coordinator.peers.get_mut(&1).unwrap().machine =
        ready_with(Machine::with_controller_enrollment());
    let principal = coordinator.peers[&1].principal;
    let retained = coordinator
        .broker
        .as_mut()
        .unwrap()
        .register_controller_session(principal, b"existing-registration")
        .unwrap();
    let target = coordinator.targets[&id.chip_id()].reference;
    let registration = festerm_automation_mcp::ControllerId::from_random_bytes([0x12; 16]);
    let registration_id = registration.to_wire();
    for operation in [
        Operation::ControllerCapabilities,
        Operation::ControllerRegister {
            local_id: "client".into(),
        },
        Operation::ControllerStatus {
            registration_id: registration,
        },
        Operation::ControllerUnregister {
            registration_id: registration,
        },
    ] {
        assert!(matches!(
            direct_binding(&operation, Some(target)),
            Err(Refusal::Unavailable)
        ));
    }

    for approved in [false, true] {
        if approved {
            coordinator.approve(1, vec![target]).unwrap();
        }
        for (tool, arguments) in [
            ("festerm_controller_capabilities", json!({})),
            (
                "festerm_controller_status",
                json!({"registration_id": registration_id.as_str()}),
            ),
            (
                "festerm_controller_unregister",
                json!({"registration_id": registration_id.as_str()}),
            ),
            (
                "festerm_controller_register",
                json!({"local_id": "untrusted-controller-must-not-leak"}),
            ),
        ] {
            call(&mut coordinator, &[&app], tool, arguments);
            let reply = response(&receive);
            assert_eq!(
                reply["result"]["structuredContent"],
                json!({"status": "refused", "code": "UNAVAILABLE"})
            );
            assert!(!reply
                .to_string()
                .contains("untrusted-controller-must-not-leak"));
            assert_eq!(coordinator.peers[&1].grant.is_some(), approved);
            assert!(coordinator.peers[&1].waits.is_empty());
            assert!(app.take_automation_command().is_none());
            assert_eq!(
                coordinator
                    .broker
                    .as_ref()
                    .unwrap()
                    .controller_registration(principal, retained)
                    .unwrap(),
                b"existing-registration"
            );
        }
        if approved {
            call(
                &mut coordinator,
                &[&app],
                "festerm_sessions_get",
                json!({"session_ref": "ab".repeat(16)}),
            );
            assert_eq!(
                response(&receive)["result"]["structuredContent"],
                json!({"status": "refused", "code": "INVALID_ARGUMENTS"})
            );
            assert!(coordinator.peers[&1].grant.is_some());
            assert!(app.take_automation_command().is_none());
        }
    }

    let broker = coordinator.broker.as_mut().unwrap();
    for index in
        0..festerm_automation::Limits::default().controller_registrations_per_connection - 1
    {
        broker
            .register_controller_session(principal, format!("capacity-probe-{index}").as_bytes())
            .unwrap();
    }
}

#[test]
fn automation_approval_labels_escape_spoofing_controls_and_mark_truncation() {
    assert_eq!(
        approval_label("café 日本語 \"terminal\" \\path"),
        "café 日本語 \"terminal\" \\path"
    );
    assert_eq!(
        approval_label("production\n\u{202e}client\u{2069}"),
        "production\\u{a}\\u{202e}client\\u{2069}"
    );
    assert_eq!(
        approval_label(&"x".repeat(129)),
        format!("{}...", "x".repeat(128))
    );
}

#[test]
fn automation_encoded_limits_count_actual_json_escaping() {
    let value = json!({"present": true, "text": "ordinary ASCII and \"quoted\" text"});
    let encoded = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        observation::bounded(value.clone(), encoded.len()).unwrap(),
        value
    );
    assert_eq!(
        observation::bounded(value, encoded.len() - 1).unwrap_err(),
        Refusal::ResponseTooLarge
    );
}

#[test]
fn automation_absent_selection_fits_a_small_actual_response_limit() {
    let (mut coordinator, app, id, receive) = fixture();
    let target = coordinator.targets[&id.chip_id()].reference;
    coordinator.approve(1, vec![target]).unwrap();
    call(
        &mut coordinator,
        &[&app],
        "festerm_terminal_get_selection",
        json!({
            "session_ref": serde_json::to_string(&target).unwrap(),
            "max_bytes": 128,
        }),
    );
    let result = response(&receive);
    assert_eq!(
        result["result"]["structuredContent"]["data"]["present"],
        false
    );
}

#[test]
fn automation_ascii_selection_uses_its_actual_encoded_size() {
    let (mut coordinator, mut app, id, receive) = fixture();
    let target = coordinator.targets[&id.chip_id()].reference;
    coordinator.approve(1, vec![target]).unwrap();
    let session = app.automation_session_mut(id);
    session.terminal.ingest(b"\x1b[2J\x1b[H");
    session.terminal.ingest(&vec![b'a'; 1200]);
    session.terminal.ingest(b"\x1b[D");
    session.view.smart_select_at_target(
        &session.terminal,
        festerm_ui_egui::SmartSelectionScope::Paragraph,
    );
    let text = session.view.selected_text(&session.terminal).unwrap();
    assert!(text.len() > (4096 - 512) / 6);
    let mut broker = coordinator.broker.take().unwrap();
    coordinator.reconcile(&mut broker, &[&app]);
    coordinator.broker = Some(broker);
    call(
        &mut coordinator,
        &[&app],
        "festerm_terminal_get_selection",
        json!({
            "session_ref": serde_json::to_string(&target).unwrap(),
            "max_bytes": 4096,
        }),
    );
    assert_eq!(
        response(&receive)["result"]["structuredContent"]["data"]["text"],
        text
    );
}

#[test]
fn automation_cancelled_startup_does_not_block_or_restart_before_cleanup() {
    let (release, blocked) = mpsc::sync_channel::<()>(1);
    let (send, result) = mpsc::sync_channel::<Result<ipc::Server, StartFailure>>(1);
    let worker = std::thread::spawn(move || {
        blocked.recv_timeout(Duration::from_secs(5)).unwrap();
        let _ = send.send(Err(StartFailure::Cancelled));
    });
    let mut coordinator = Coordinator::default();
    coordinator.mcp_starting = Some(Starting {
        result,
        thread: worker,
    });
    coordinator.stop = Arc::new(AtomicBool::new(false));
    let old_stop = coordinator.stop.clone();
    coordinator.privacy_gate = Arc::new(PrivacyGate::new(false));
    assert!(coordinator.privacy_gate.bind_mcp(&old_stop));
    coordinator.broker = Some(Broker::new([1; 16]));
    coordinator.enabled = true;
    let context = egui::Context::default();
    let before = Instant::now();
    coordinator.restart_with_bootstrap(&context, Ok(()));
    assert!(before.elapsed() < Duration::from_secs(1));
    assert!(old_stop.load(Ordering::Acquire));
    assert!(coordinator.mcp_starting.is_none());
    assert!(coordinator.want_start);
    assert!(coordinator.mcp_retiring.thread.is_some());
    assert!(coordinator.broker.is_some());
    release.send(()).unwrap();
    assert!(coordinator
        .mcp_retiring
        .thread
        .take()
        .unwrap()
        .join()
        .unwrap());
    coordinator.want_start = false;
}

#[test]
fn automation_cleanup_spawn_failure_retains_resources_and_blocks_restart() {
    struct DropMarker(Arc<AtomicBool>);
    impl Drop for DropMarker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    let dropped = Arc::new(AtomicBool::new(false));
    let resource = DropMarker(dropped.clone());
    let (release, blocked) = mpsc::sync_channel::<()>(1);
    let mut retiring = Retirement::default();
    retiring.thread = Some(std::thread::spawn(move || {
        blocked.recv_timeout(Duration::from_secs(5)).unwrap();
        true
    }));
    retire_with(
        &mut retiring,
        "test-cleanup",
        move || {
            drop(resource);
            true
        },
        |_, _| Err(std::io::Error::other("injected spawn failure")),
    );
    assert!(!dropped.load(Ordering::Acquire));
    assert!(retiring.failed);
    assert_eq!(retiring.stranded.len(), 1);
    assert!(!cleanup_ready(
        &mut retiring,
        &egui::Context::default(),
        true
    ));

    release.send(()).unwrap();
    let stranded = retiring.stranded.pop().unwrap();
    let cleanup = stranded.lock().unwrap().take().unwrap();
    assert!(cleanup());
    assert!(dropped.load(Ordering::Acquire));

    let mut coordinator = Coordinator::default();
    coordinator.mcp_retiring = retiring;
    coordinator.broker = Some(Broker::new([1; 16]));
    coordinator.privacy_gate = Arc::new(PrivacyGate::new(false));
    coordinator.enabled = true;
    coordinator.restart_with_bootstrap(&egui::Context::default(), Ok(()));
    assert_eq!(coordinator.status, CLEANUP_FAILED);
    assert!(coordinator.stop.load(Ordering::Acquire));
    assert!(coordinator.mcp_starting.is_none());
}

#[test]
fn automation_cleanup_panic_blocks_shared_and_mcp_restarts() {
    for shared in [false, true] {
        for chained in [false, true] {
            let mut coordinator = Coordinator::default();
            let retiring = if shared {
                &mut coordinator.retiring
            } else {
                &mut coordinator.mcp_retiring
            };
            retiring.thread = Some(std::thread::spawn(|| panic!("injected cleanup failure")));
            if chained {
                retire(retiring, "test-cleanup", || true);
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            while !retiring.thread.as_ref().unwrap().is_finished() {
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
            if !shared {
                coordinator.broker = Some(Broker::new([1; 16]));
                coordinator.privacy_gate = Arc::new(PrivacyGate::new(false));
            }
            coordinator.enabled = true;
            coordinator.restart_with_bootstrap(&egui::Context::default(), Ok(()));
            assert_eq!(coordinator.status, CLEANUP_FAILED);
            assert!(coordinator.stop.load(Ordering::Acquire));
            assert!(coordinator.starting.is_none());
            assert!(coordinator.mcp_starting.is_none());
        }
    }
}

#[test]
fn automation_stopping_mcp_does_not_cancel_shared_startup() {
    let (release, blocked) = mpsc::sync_channel::<()>(1);
    let (send, result) = mpsc::sync_channel::<Result<Prepared, StartFailure>>(1);
    let worker = std::thread::spawn(move || {
        blocked.recv_timeout(Duration::from_secs(5)).unwrap();
        let _ = send.send(Err(StartFailure::Privacy));
    });
    let mut coordinator = Coordinator::default();
    coordinator.starting = Some(Starting {
        result,
        thread: worker,
    });
    coordinator.privacy_gate = Arc::new(PrivacyGate::new(false));
    coordinator.stop = Arc::new(AtomicBool::new(false));
    assert!(coordinator.privacy_gate.bind_mcp(&coordinator.stop));

    coordinator.stop_mcp();
    assert!(coordinator.stop.load(Ordering::Acquire));
    assert!(!coordinator.privacy_gate.is_closed());
    assert!(coordinator.starting.is_some());
    release.send(()).unwrap();
    let starting = coordinator.starting.take().unwrap();
    assert!(matches!(
        starting.result.recv_timeout(Duration::from_secs(5)),
        Ok(Err(StartFailure::Privacy))
    ));
    starting.thread.join().unwrap();
}

#[test]
fn automation_mcp_loss_preserves_independent_broker_state() {
    for transport_failure in [false, true] {
        let (mut coordinator, app, id, _) = fixture();
        let target = coordinator.targets[&id.chip_id()].reference;
        coordinator.approve(1, vec![target]).unwrap();
        let peer = coordinator.peers[&1].connection.clone();
        let mcp_principal = coordinator.peers[&1].principal;
        let (independent, run, now) = independent_run(&mut coordinator, target);
        let registration = coordinator.targets[&id.chip_id()].registration.clone();

        if transport_failure {
            coordinator.mcp_failed();
        } else {
            coordinator.tick(&egui::Context::default(), &[&app]);
            assert_eq!(
                coordinator.settings(&[&app]).status,
                SettingsSnapshot::default().status
            );
        }

        assert!(peer.is_closed());
        assert!(coordinator.peers.is_empty());
        assert!(!coordinator.privacy_gate.is_closed());
        assert!(registration.load(Ordering::Acquire));
        assert_eq!(coordinator.targets[&id.chip_id()].reference, target);
        let broker = coordinator.broker.as_mut().unwrap();
        assert_eq!(
            broker.status(independent, run, now).unwrap().state,
            festerm_automation::RunState::Running
        );
        assert!(broker
            .request_approval(
                mcp_principal,
                GrantRequest {
                    scope: Scope::default(),
                    bindings: BTreeSet::new(),
                    direct_read_calls: BTreeSet::from([DirectKind::Discovery]),
                },
                now,
            )
            .is_err());
        let snapshot = coordinator.settings(&[&app]);
        assert!(snapshot.channels.is_empty());
        assert!(snapshot.targets.is_empty());

        coordinator.start(&egui::Context::default());
        assert_eq!(
            coordinator.status,
            start_failure_status(StartFailure::PeerIdentity)
        );
        assert!(coordinator
            .broker
            .as_mut()
            .unwrap()
            .status(independent, run, now)
            .is_ok());
    }
}

#[test]
fn automation_privacy_loss_revokes_every_principal_and_invalidates_targets() {
    let (mut coordinator, app, id, _) = fixture();
    let target = coordinator.targets[&id.chip_id()].reference;
    coordinator.approve(1, vec![target]).unwrap();
    let (independent, run, now) = independent_run(&mut coordinator, target);
    let registration = coordinator.targets[&id.chip_id()].registration.clone();
    let mcp_principal = coordinator.peers[&1].principal;
    let operation = Operation::TerminalReadScreen {
        session_ref: serde_json::to_string(&target).unwrap(),
        max_bytes: 1024,
    };
    let (_, pending, _) = admit_request(
        coordinator.broker.as_mut().unwrap(),
        &coordinator.targets,
        &coordinator.peers[&1],
        &operation,
        now,
    )
    .unwrap();

    coordinator.privacy_gate.close();
    assert!(coordinator.stop.load(Ordering::Acquire));
    let snapshot = coordinator.settings(&[&app]);
    assert!(snapshot.status.contains("admission is closed"));
    assert!(snapshot.channels.is_empty());
    assert!(snapshot.targets.is_empty());
    coordinator.close_shared_admission();
    assert!(!registration.load(Ordering::Acquire));
    let broker = coordinator.broker.as_mut().unwrap();
    assert!(broker.status(independent, run, now).is_err());
    assert!(broker
        .commit_disclosure(mcp_principal, pending, 1, now)
        .is_err());

    coordinator.privacy_lost();
    assert!(coordinator.broker.is_none());
    assert!(coordinator.targets.is_empty());
    assert!(coordinator.peers.is_empty());
}

#[test]
fn automation_privacy_gate_closes_replaced_and_later_mcp_stops() {
    let gate = PrivacyGate::new(false);
    let first = Arc::new(AtomicBool::new(false));
    let second = Arc::new(AtomicBool::new(false));
    assert!(gate.bind_mcp(&first));
    assert!(gate.bind_mcp(&second));
    assert!(first.load(Ordering::Acquire));
    assert!(!second.load(Ordering::Acquire));
    gate.close();
    assert!(second.load(Ordering::Acquire));
    let late = Arc::new(AtomicBool::new(false));
    assert!(!gate.bind_mcp(&late));
    assert!(late.load(Ordering::Acquire));
}

#[test]
fn automation_queued_restart_cannot_cross_privacy_loss() {
    let context = egui::Context::default();
    let settings = festerm_config::InterfaceSettings::DEFAULT.with_mcp_enabled(true);
    let configuration = festerm_config::Configuration::empty()
        .with_interface_settings(settings)
        .unwrap();
    let app = FesTermApp::for_test_with_configuration(configuration);
    for loss_processed in [false, true] {
        let (mut coordinator, _, _, _) = fixture();
        coordinator.privacy_gate.close();
        if loss_processed {
            coordinator.privacy_lost();
        }
        coordinator.command(NativeCommand::RestartMcp, &context);
        assert!(coordinator.broker.is_none());
        assert!(!coordinator.want_start);
        assert!(coordinator
            .status
            .contains("all automation grants were revoked"));
        coordinator.command(
            NativeCommand::ApproveObservation {
                channel: 1,
                targets: Vec::new(),
            },
            &context,
        );
        assert!(coordinator
            .status
            .contains("all automation grants were revoked"));

        coordinator.tick(&context, &[&app]);
        coordinator.command(NativeCommand::RestartMcp, &context);
        assert_eq!(
            coordinator.status,
            start_failure_status(StartFailure::PeerIdentity)
        );
        assert!(coordinator.broker.is_none());
    }
}

#[test]
fn automation_cancelled_shared_start_discards_queued_restart() {
    let context = egui::Context::default();
    let settings = festerm_config::InterfaceSettings::DEFAULT.with_mcp_enabled(true);
    let configuration = festerm_config::Configuration::empty()
        .with_interface_settings(settings)
        .unwrap();
    let app = FesTermApp::for_test_with_configuration(configuration);
    let (send, result) = mpsc::sync_channel::<Result<Prepared, StartFailure>>(1);
    assert!(send.send(Err(StartFailure::Cancelled)).is_ok());
    let mut coordinator = Coordinator::default();
    coordinator.starting = Some(Starting {
        result,
        thread: std::thread::spawn(|| {}),
    });
    coordinator.privacy_gate = Arc::new(PrivacyGate::new(false));
    coordinator.stop = Arc::new(AtomicBool::new(false));
    assert!(coordinator.privacy_gate.bind_mcp(&coordinator.stop));
    coordinator.enabled = true;
    coordinator.want_start = true;
    coordinator.privacy_gate.close();

    coordinator.tick(&context, &[&app]);
    coordinator.command(NativeCommand::RestartMcp, &context);
    assert_eq!(
        coordinator.status,
        start_failure_status(StartFailure::Cancelled)
    );
    assert!(coordinator.broker.is_none());

    coordinator.tick(&context, &[&app]);
    coordinator.command(NativeCommand::RestartMcp, &context);
    assert_eq!(
        coordinator.status,
        start_failure_status(StartFailure::PeerIdentity)
    );
}

#[test]
fn automation_shared_start_refuses_privacy_or_randomness_failure() {
    let gate = PrivacyGate::new(false);
    let stop = Arc::new(AtomicBool::new(false));
    assert!(gate.bind_mcp(&stop));
    assert_eq!(
        prepare_shared(&gate, || Ok(()), || Err(StartFailure::Randomness)),
        Err(StartFailure::Randomness)
    );
    assert!(gate.is_closed());
    assert!(stop.load(Ordering::Acquire));

    let gate = PrivacyGate::new(false);
    assert_eq!(
        prepare_shared(
            &gate,
            || Err::<(), _>(StartFailure::Privacy),
            || panic!("randomness must not run without privacy monitoring"),
        ),
        Err(StartFailure::Privacy)
    );
    assert!(gate.is_closed());
}

#[test]
fn automation_preferences_do_not_start_unqualified_or_javascript_adapters() {
    for mcp in [false, true] {
        let settings = festerm_config::InterfaceSettings::DEFAULT
            .with_javascript_enabled(true)
            .with_mcp_enabled(mcp);
        let configuration = festerm_config::Configuration::empty()
            .with_interface_settings(settings)
            .unwrap();
        let app = FesTermApp::for_test_with_configuration(configuration);
        let mut coordinator = Coordinator::default();
        coordinator.tick(&egui::Context::default(), &[&app]);
        assert!(coordinator.broker.is_none());
        assert!(coordinator.privacy.is_none());
        assert!(coordinator.starting.is_none());
        assert!(coordinator.mcp_starting.is_none());
        assert!(coordinator.server.is_none());
        if mcp {
            assert_eq!(
                coordinator.status,
                start_failure_status(StartFailure::PeerIdentity)
            );
        } else {
            assert_eq!(coordinator.status, SettingsSnapshot::default().status);
        }
    }
}

#[test]
fn automation_settings_presentation_changes_need_a_followup_frame() {
    let (mut app, _) = FesTermApp::automation_fixture();
    let pending = SettingsSnapshot::with_pending_channel_for_test(7);
    assert!(app.set_automation_settings(pending.clone()));
    assert!(!app.set_automation_settings(pending.clone()));
    let mut approved = pending;
    approved.channels[0].approved = true;
    assert!(app.set_automation_settings(approved));
    assert!(app.set_automation_settings(SettingsSnapshot::default()));
}

#[test]
fn automation_selected_workflow_preview_requires_opt_in_and_grants_no_access() {
    use festerm_automation_js::snapshot::{Manifest, SourceSnapshot};

    let directory = tempfile::tempdir().unwrap();
    #[cfg(windows)]
    let root = directory.path().to_path_buf();
    #[cfg(not(windows))]
    let root = directory.path().canonicalize().unwrap();
    let path = root.join("workflow.json");
    let snapshot = SourceSnapshot {
        manifest: Manifest {
            version: 1,
            api_version: 1,
            name: "test".into(),
            entry: "main.js".into(),
            modules: vec!["main.js".into()],
            parameters: BTreeMap::new(),
            capabilities: BTreeSet::new(),
        },
        modules: BTreeMap::from([("main.js".into(), "export default 1;".into())]),
    };
    std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();

    let context = egui::Context::default();
    let mut coordinator = Coordinator::default();
    coordinator.command(
        NativeCommand::CaptureWorkflow(SelectedWorkflow(path.clone())),
        &context,
    );
    assert!(matches!(
        coordinator.workflow.view().status,
        WorkflowStatus::Failed(_)
    ));

    let configuration = festerm_config::Configuration::empty()
        .with_interface_settings(
            festerm_config::InterfaceSettings::DEFAULT.with_javascript_enabled(true),
        )
        .unwrap();
    let enabled = FesTermApp::for_test_with_configuration(configuration);
    let disabled = FesTermApp::for_test_with_configuration(festerm_config::Configuration::empty());
    coordinator.tick(&context, &[&enabled, &disabled]);
    coordinator.command(
        NativeCommand::CaptureWorkflow(SelectedWorkflow(path.clone())),
        &context,
    );
    assert!(matches!(
        coordinator.workflow.view().status,
        WorkflowStatus::Failed(_)
    ));
    coordinator.tick(&context, &[&enabled]);
    assert!(coordinator.commands_for_test(
        vec![
            NativeCommand::CaptureWorkflow(SelectedWorkflow(path.clone())),
            NativeCommand::CaptureWorkflow(SelectedWorkflow(path.clone())),
        ],
        &context,
        &[&enabled],
    ));
    assert_eq!(coordinator.workflow.view().status, WorkflowStatus::Empty);
    coordinator.tick(&context, &[&enabled]);
    assert!(!coordinator.commands_for_test(
        vec![NativeCommand::CaptureWorkflow(SelectedWorkflow(path))],
        &context,
        &[&enabled],
    ));
    let deadline = Instant::now() + Duration::from_secs(5);
    while coordinator.workflow.view().busy() {
        coordinator.tick(&context, &[&enabled]);
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let preview = coordinator.settings(&[&enabled]).workflow;
    assert_eq!(preview.status, WorkflowStatus::Captured);
    assert_eq!(preview.current_bundle().unwrap().manifest().name, "test");
    assert!(coordinator.broker.is_none());
    assert!(coordinator.server.is_none());

    coordinator.workflow.view().disable();
    coordinator.tick(&context, &[&disabled]);
    assert_eq!(coordinator.workflow.view().status, WorkflowStatus::Empty);
    assert!(coordinator.workflow.view().current_bundle().is_none());
}

#[test]
fn automation_restart_stops_existing_channels_before_coordinator_dispatch() {
    let (mut app, _) = FesTermApp::automation_fixture();
    let stop = Arc::new(AtomicBool::new(false));
    app.set_automation_settings(SettingsSnapshot {
        stop: stop.clone(),
        ..SettingsSnapshot::default()
    });
    app.queue_automation_command(NativeCommand::RestartMcp);
    assert!(stop.load(Ordering::Acquire));
    assert!(matches!(
        app.take_automation_command(),
        Some(NativeCommand::RestartMcp)
    ));
}

#[test]
fn automation_same_pass_cancellation_suppresses_approval_and_capture() {
    let context = egui::Context::default();
    for revoke in [false, true] {
        for cancellation_first in [false, true] {
            let (mut coordinator, app, id, _receive) = fixture();
            let target = coordinator.targets[&id.chip_id()].reference;
            let approval = NativeCommand::ApproveObservation {
                channel: 1,
                targets: vec![target],
            };
            let cancellation = if revoke {
                NativeCommand::Revoke { channel: 1 }
            } else {
                NativeCommand::ClearWorkflow
            };
            let commands = if cancellation_first {
                vec![cancellation, approval]
            } else {
                vec![approval, cancellation]
            };
            assert!(coordinator.commands_for_test(commands, &context, &[&app]));
            assert!(coordinator.peers[&1].grant.is_none());
            assert_eq!(coordinator.peers[&1].connection.is_closed(), revoke);
            assert!(coordinator.status.contains("blocked"));
        }
    }

    for clear_first in [false, true] {
        let (mut coordinator, app, _, _receive) = fixture();
        let capture = NativeCommand::CaptureWorkflow(SelectedWorkflow("unused.json".into()));
        let commands = if clear_first {
            vec![NativeCommand::ClearWorkflow, capture]
        } else {
            vec![capture, NativeCommand::ClearWorkflow]
        };
        assert!(coordinator.commands_for_test(commands, &context, &[&app]));
        assert!(matches!(
            coordinator.workflow.view().status,
            WorkflowStatus::Empty
        ));
        assert!(coordinator.status.contains("blocked"));
    }
}

#[test]
fn automation_competing_workflow_captures_cancel_pending_without_winner() {
    use festerm_automation_js::snapshot::{ApprovedBundle, Manifest, SourceSnapshot};

    let context = egui::Context::default();
    for identical in [false, true] {
        for reverse in [false, true] {
            let first = std::path::PathBuf::from("first.json");
            let second = if identical {
                first.clone()
            } else {
                "second.json".into()
            };
            let commands = || {
                let mut commands = vec![
                    NativeCommand::CaptureWorkflow(SelectedWorkflow(first.clone())),
                    NativeCommand::CaptureWorkflow(SelectedWorkflow(second.clone())),
                ];
                if reverse {
                    commands.reverse();
                }
                commands
            };
            {
                let (mut coordinator, app, _, _receive) = fixture();
                coordinator.javascript_enabled = true;
                assert!(coordinator.commands_for_test(commands(), &context, &[&app]));
                let snapshot = coordinator.settings(&[&app]);
                assert_eq!(snapshot.workflow.status, WorkflowStatus::Empty);
                assert!(snapshot.workflow.current_bundle().is_none());
                assert!(snapshot.status.contains("conflicted"));
            }

            let (mut coordinator, app, _, _receive) = fixture();
            coordinator.javascript_enabled = true;
            let bundle = ApprovedBundle::new(SourceSnapshot {
                manifest: Manifest {
                    version: 1,
                    api_version: 1,
                    name: "pending".into(),
                    entry: "main.js".into(),
                    modules: vec!["main.js".into()],
                    parameters: BTreeMap::new(),
                    capabilities: BTreeSet::new(),
                },
                modules: BTreeMap::from([("main.js".into(), "export default 1;".into())]),
            })
            .unwrap();
            let complete_capture = coordinator.workflow.pending_capture_for_test();
            assert_eq!(
                coordinator.workflow.view().status,
                WorkflowStatus::Capturing
            );

            assert!(coordinator.commands_for_test(commands(), &context, &[&app]));
            let snapshot = coordinator.settings(&[&app]);
            assert_eq!(snapshot.workflow.status, WorkflowStatus::Cancelling);
            assert!(snapshot.workflow.current_bundle().is_none());
            assert!(snapshot.status.contains("conflicted"));
            assert!(coordinator.stop.load(Ordering::Acquire));

            complete_capture.send(Ok(bundle)).unwrap();
            coordinator.workflow.poll(&context);
            let snapshot = coordinator.settings(&[&app]);
            assert_eq!(snapshot.workflow.status, WorkflowStatus::Empty);
            assert!(snapshot.workflow.current_bundle().is_none());
            assert!(snapshot.status.contains("conflicted"));
        }
    }
}

#[test]
fn automation_command_queue_overflow_stops_admission_before_settlement() {
    let (coordinator, mut app, id, _receive) = fixture();
    let target = coordinator.targets[&id.chip_id()].reference;
    let stop = coordinator.stop.clone();
    app.set_automation_settings(coordinator.settings(&[&app]));
    for _ in 0..=MAX_NATIVE_COMMANDS {
        app.queue_automation_command(NativeCommand::ApproveObservation {
            channel: 1,
            targets: vec![target],
        });
    }
    assert!(stop.load(Ordering::Acquire));
    let (commands, overflow) = app.take_automation_commands();
    assert!(commands.is_empty());
    assert!(overflow);
}

#[test]
fn automation_target_replacement_invalidates_queued_responses_immediately() {
    let (mut coordinator, mut app, id, _) = fixture();
    let target = coordinator.targets[&id.chip_id()].reference;
    coordinator.approve(1, vec![target]).unwrap();
    let guard = coordinator.peers[&1]
        .grant
        .as_ref()
        .unwrap()
        .target_liveness[0]
        .clone();
    assert!(guard.liveness.load(Ordering::Acquire));
    app.automation_session_mut(id)
        .controller
        .advance_lifecycle_generation();
    assert!(!guard.liveness.load(Ordering::Acquire));
    let mut broker = coordinator.broker.take().unwrap();
    coordinator.reconcile(&mut broker, &[&app]);
    assert_ne!(target, coordinator.targets[&id.chip_id()].reference);
    coordinator.broker = Some(broker);
}

#[test]
fn automation_backend_replacement_revokes_even_without_lifecycle_events() {
    let (mut coordinator, app, id, receive) = fixture();
    let target = coordinator.targets[&id.chip_id()].reference;
    coordinator.approve(1, vec![target]).unwrap();
    let session = app.automation_session(id).unwrap();
    let Some(crate::tabs::ApplicationSession::TestSsh(fake)) = session.controller.session() else {
        panic!("expected fake SSH attachment");
    };
    fake.replace_attachment_without_event();

    call(
        &mut coordinator,
        &[&app],
        "festerm_terminal_read_screen",
        json!({"session_ref": serde_json::to_string(&target).unwrap()}),
    );
    assert!(receive().is_none());
    assert!(coordinator.peers[&1].connection.is_closed());
}

#[test]
fn automation_revision_exhaustion_invalidates_queued_responses() {
    let (mut coordinator, mut app, id, _) = fixture();
    let target = coordinator.targets[&id.chip_id()].reference;
    coordinator.approve(1, vec![target]).unwrap();
    let guard = coordinator.peers[&1]
        .grant
        .as_ref()
        .unwrap()
        .target_liveness[0]
        .clone();
    coordinator.targets.get_mut(&id.chip_id()).unwrap().revision = u64::MAX;
    app.automation_session_mut(id)
        .controller
        .record_local_observation_change();
    let mut broker = coordinator.broker.take().unwrap();
    coordinator.reconcile(&mut broker, &[&app]);
    assert!(!coordinator.targets.contains_key(&id.chip_id()));
    assert!(!guard.registration.load(Ordering::Acquire));
    coordinator.broker = Some(broker);
}

#[test]
fn automation_direct_invocations_bind_exact_validated_parameters() {
    let (coordinator, _app, id, _) = fixture();
    let target = coordinator.targets[&id.chip_id()].reference;
    let read = Operation::TerminalReadScreen {
        session_ref: "opaque reference".into(),
        max_bytes: 4096,
    };
    let binding = direct_binding(&read, Some(target)).unwrap();
    let different_limit = Operation::TerminalReadScreen {
        session_ref: "opaque reference".into(),
        max_bytes: 2048,
    };
    let selection = Operation::TerminalGetSelection {
        session_ref: "opaque reference".into(),
        max_bytes: 4096,
    };
    assert_ne!(
        binding.parameters,
        direct_binding(&different_limit, Some(target))
            .unwrap()
            .parameters
    );
    assert_ne!(
        binding.parameters,
        direct_binding(&selection, Some(target)).unwrap().parameters
    );
    let other_target = Broker::new([2; 16]).attach_target(99, 1).unwrap();
    assert_ne!(
        binding.parameters,
        direct_binding(&read, Some(other_target))
            .unwrap()
            .parameters
    );
    let history = |cursor: Option<&str>| Operation::TerminalReadScrollback {
        session_ref: "opaque reference".into(),
        cursor: cursor.map(str::to_owned),
        max_bytes: 4096,
    };
    assert_ne!(
        direct_binding(&history(None), Some(target))
            .unwrap()
            .parameters,
        direct_binding(&history(Some("1:2:3:4")), Some(target))
            .unwrap()
            .parameters
    );
    let wait = |after_revision, timeout_ms| Operation::TerminalWaitForChange {
        session_ref: "opaque reference".into(),
        after_revision,
        timeout_ms,
    };
    let initial_wait = direct_binding(&wait(1, 200), Some(target))
        .unwrap()
        .parameters;
    assert_ne!(
        initial_wait,
        direct_binding(&wait(2, 200), Some(target))
            .unwrap()
            .parameters
    );
    assert_ne!(
        initial_wait,
        direct_binding(&wait(1, 300), Some(target))
            .unwrap()
            .parameters
    );
    let invalid_cursor = Operation::TerminalReadScrollback {
        session_ref: "opaque reference".into(),
        cursor: Some("01:0:0:0".into()),
        max_bytes: 4096,
    };
    assert!(matches!(
        direct_binding(&invalid_cursor, Some(target)),
        Err(Refusal::InvalidArguments)
    ));
}

#[test]
fn automation_pairing_discloses_no_targets_and_never_grants_a_channel() {
    let (mut coordinator, app, _, receive) = fixture();
    call(
        &mut coordinator,
        &[&app],
        "festerm_pairing_status",
        json!({}),
    );
    let status = response(&receive);
    assert!(!status.to_string().contains("approved source"));
    assert!(!status.to_string().contains("automation.example.test"));
    assert!(coordinator.peers[&1].grant.is_none());
    call(
        &mut coordinator,
        &[&app],
        "festerm_sessions_list",
        json!({}),
    );
    assert_eq!(
        response(&receive)["result"]["structuredContent"]["code"],
        "NOT_AUTHORIZED"
    );
}

#[test]
fn automation_native_observation_approval_is_exact_and_channel_owned() {
    let (mut coordinator, app, id, receive) = fixture();
    let (other, _) = FesTermApp::automation_fixture();
    let mut broker = coordinator.broker.take().unwrap();
    coordinator.reconcile(&mut broker, &[&app, &other]);
    coordinator.broker = Some(broker);
    let target = coordinator.targets[&id.chip_id()].reference;
    coordinator.approve(1, vec![target]).unwrap();
    let settings = coordinator.settings(&[&app, &other]);
    assert_eq!(settings.channels[0].approved_targets, [target]);
    assert!(settings
        .targets
        .iter()
        .any(|(candidate, _)| *candidate == target));
    call(
        &mut coordinator,
        &[&app, &other],
        "festerm_sessions_list",
        json!({}),
    );
    let list = response(&receive);
    let sessions = list["result"]["structuredContent"]["data"]["sessions"]
        .as_array()
        .unwrap();
    assert_eq!(sessions.len(), 1);
    let reference = sessions[0]["session_ref"].as_str().unwrap();
    assert_eq!(
        serde_json::from_str::<TargetRef>(reference).unwrap(),
        target
    );
    call(
        &mut coordinator,
        &[&app, &other],
        "festerm_terminal_read_screen",
        json!({
            "session_ref": reference, "max_bytes": 4096,
        }),
    );
    let read = response(&receive);
    assert!(read["result"]["structuredContent"]["data"]["text"]
        .as_str()
        .unwrap()
        .starts_with("approved source"));
    let wrong = coordinator
        .targets
        .values()
        .find(|candidate| candidate.reference != target)
        .unwrap()
        .reference;
    call(
        &mut coordinator,
        &[&app, &other],
        "festerm_terminal_read_screen",
        json!({
            "session_ref": serde_json::to_string(&wrong).unwrap(),
        }),
    );
    assert_eq!(
        response(&receive)["result"]["structuredContent"]["code"],
        "NOT_AUTHORIZED"
    );
    let copied = serde_json::to_string(&target).unwrap();
    let peer = coordinator.peers.get_mut(&1).unwrap();
    peer.grant = None;
    call(
        &mut coordinator,
        &[&app],
        "festerm_terminal_read_screen",
        json!({"session_ref": copied}),
    );
    assert_eq!(
        response(&receive)["result"]["structuredContent"]["code"],
        "NOT_AUTHORIZED"
    );
}

#[test]
fn automation_waits_are_notified_cancelled_and_cannot_cross_replacement() {
    let (mut coordinator, mut app, id, receive) = fixture();
    let target = coordinator.targets[&id.chip_id()].reference;
    coordinator.approve(1, vec![target]).unwrap();
    let reference = serde_json::to_string(&target).unwrap();
    let key = call(
        &mut coordinator,
        &[&app],
        "festerm_terminal_wait_for_change",
        json!({
            "session_ref": reference, "after_revision": 1, "timeout_ms": 30_000,
        }),
    );
    assert!(receive().is_none());
    let mut broker = coordinator.broker.take().unwrap();
    cancel_wait(
        &mut broker,
        coordinator.peers.get_mut(&1).unwrap(),
        &key,
        Instant::now(),
    );
    assert!(coordinator.peers[&1].waits.is_empty());
    coordinator.peers.get_mut(&1).unwrap().machine.close();
    coordinator.peers.get_mut(&1).unwrap().machine = ready();
    coordinator.broker = Some(broker);
    call(
        &mut coordinator,
        &[&app],
        "festerm_terminal_wait_for_change",
        json!({
            "session_ref": reference, "after_revision": 1, "timeout_ms": 30_000,
        }),
    );
    let session = app.automation_session_mut(id);
    session.terminal.ingest(b"changed");
    session.controller.record_local_observation_change();
    let mut broker = coordinator.broker.take().unwrap();
    coordinator.reconcile(&mut broker, &[&app]);
    settle_waits(
        &mut broker,
        &coordinator.targets,
        coordinator.peers.get_mut(&1).unwrap(),
        Instant::now(),
    );
    assert_eq!(
        response(&receive)["result"]["structuredContent"]["data"]["changed"],
        true
    );
    app.automation_session_mut(id)
        .controller
        .advance_lifecycle_generation();
    coordinator.reconcile(&mut broker, &[&app]);
    assert_ne!(coordinator.targets[&id.chip_id()].reference, target);
    coordinator.broker = Some(broker);
    call(
        &mut coordinator,
        &[&app],
        "festerm_terminal_read_screen",
        json!({"session_ref": reference}),
    );
    assert_eq!(
        response(&receive)["result"]["structuredContent"]["code"],
        "STALE_TARGET"
    );
}

#[test]
fn automation_wait_observes_applied_persistent_recovery_sync() {
    let (mut coordinator, mut app, id, receive) = fixture();
    let target = coordinator.targets[&id.chip_id()].reference;
    coordinator.approve(1, vec![target]).unwrap();
    let reference = serde_json::to_string(&target).unwrap();
    call(
        &mut coordinator,
        &[&app],
        "festerm_terminal_wait_for_change",
        json!({
            "session_ref": reference, "after_revision": 1, "timeout_ms": 30_000,
        }),
    );
    assert!(receive().is_none());

    let session = app.automation_session_mut(id);
    let Some(crate::tabs::ApplicationSession::TestSsh(fake)) = session.controller.session() else {
        panic!("expected fake SSH attachment");
    };
    let payload = festerm_sessiond::encode_recovery_sync_command(
        &festerm_sessiond::RecoverySyncCommand::MirrorBytes(b"\x1b[2J\x1b[Hnew".to_vec()),
    )
    .unwrap();
    fake.push_event(festerm_session::SessionEvent::RecoverySync(payload));
    session.controller.pump_events(&mut session.terminal);

    let mut broker = coordinator.broker.take().unwrap();
    coordinator.reconcile(&mut broker, &[&app]);
    settle_waits(
        &mut broker,
        &coordinator.targets,
        coordinator.peers.get_mut(&1).unwrap(),
        Instant::now(),
    );
    assert_eq!(
        response(&receive)["result"]["structuredContent"]["data"]["changed"],
        true
    );
    coordinator.broker = Some(broker);
}

#[test]
fn automation_grant_does_not_follow_replaced_controller_with_same_tab_and_generation() {
    let (mut coordinator, mut app, id, receive) = fixture();
    let old_target = coordinator.targets[&id.chip_id()].reference;
    let old_generation = app
        .automation_session(id)
        .unwrap()
        .controller
        .lifecycle_generation();
    coordinator.approve(1, vec![old_target]).unwrap();

    assert_eq!(app.automation_replace_fixture_session(), id);
    assert_eq!(
        app.automation_session(id)
            .unwrap()
            .controller
            .lifecycle_generation(),
        old_generation
    );
    app.automation_session_mut(id)
        .terminal
        .ingest(b"replacement secret");
    let mut broker = coordinator.broker.take().unwrap();
    coordinator.reconcile(&mut broker, &[&app]);
    assert_ne!(coordinator.targets[&id.chip_id()].reference, old_target);
    coordinator.broker = Some(broker);

    call(
        &mut coordinator,
        &[&app],
        "festerm_terminal_read_screen",
        json!({
            "session_ref": serde_json::to_string(&old_target).unwrap(),
        }),
    );
    let reply = response(&receive);
    assert_eq!(reply["result"]["structuredContent"]["code"], "STALE_TARGET");
    assert!(!reply.to_string().contains("replacement secret"));
}

#[test]
fn automation_closed_tabs_do_not_exhaust_target_registrations() {
    let (mut coordinator, _app, _, _) = fixture();
    let mut first = None;
    for _ in 0..130 {
        let (app, id) = FesTermApp::automation_fixture();
        let mut broker = coordinator.broker.take().unwrap();
        coordinator.reconcile(&mut broker, &[&app]);
        let target = coordinator.targets[&id.chip_id()].reference;
        assert_eq!(coordinator.targets.len(), 1);
        if let Some(first) = first {
            assert_ne!(target, first);
            assert_eq!(
                broker.close_target(first),
                Err(festerm_automation::Error::StaleTarget)
            );
        } else {
            first = Some(target);
        }
        coordinator.broker = Some(broker);
    }
}

#[test]
fn automation_disclosure_and_authority_exhaustion_are_explicit_refusals() {
    let (mut coordinator, app, id, receive) = fixture();
    let target = coordinator.targets[&id.chip_id()].reference;
    coordinator.approve(1, vec![target]).unwrap();
    let reference = serde_json::to_string(&target).unwrap();
    for _ in 0..32 {
        call(
            &mut coordinator,
            &[&app],
            "festerm_terminal_read_screen",
            json!({"session_ref": reference}),
        );
        assert_eq!(
            response(&receive)["result"]["structuredContent"]["status"],
            "ok"
        );
    }
    call(
        &mut coordinator,
        &[&app],
        "festerm_terminal_read_screen",
        json!({"session_ref": reference}),
    );
    assert_eq!(
        response(&receive)["result"]["structuredContent"]["code"],
        "BUDGET_EXCEEDED"
    );
    coordinator.shutdown();
    assert!(coordinator.peers.is_empty());
    assert!(coordinator.stop.load(Ordering::Acquire));
}
