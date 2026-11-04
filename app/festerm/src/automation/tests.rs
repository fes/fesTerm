use super::*;
use serde_json::{json, Value};

fn ready() -> Machine {
    let mut machine = Machine::new();
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
        },
    );
    coordinator.broker = Some(broker);
    (coordinator, app, id, receive)
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
fn automation_approval_labels_escape_spoofing_controls_and_mark_truncation() {
    assert_eq!(
        approval_label("production\n\u{202e}client\u{2069}"),
        "production\\u{a}\\u{202e}client\\u{2069}"
    );
    assert_eq!(approval_label(r"literal\u{202e}"), r"literal\\u{202e}");
    assert_eq!(
        review_label("combining\u{301}\u{200d}\\u{202e}"),
        r"combining\u{301}\u{200d}\\u{202e}"
    );
    assert_eq!(
        approval_label(&"x".repeat(129)),
        format!("{}...", "x".repeat(128))
    );
}

fn workflow_review_bundle() -> festerm_automation_js::snapshot::ApprovedBundle {
    use festerm_automation_js::snapshot::{ApprovedBundle, Capability, Manifest, SourceSnapshot};
    ApprovedBundle::new(SourceSnapshot {
        manifest: Manifest {
            version: 1,
            api_version: 1,
            name: "test".into(),
            entry: "main.js".into(),
            modules: vec!["main.js".into()],
            parameters: BTreeMap::new(),
            capabilities: BTreeSet::from([Capability::ReadScreen]),
        },
        modules: BTreeMap::from([("main.js".into(), "export default 1;".into())]),
    })
    .unwrap()
}

#[test]
fn workflow_review_uses_shared_targets_without_mcp_disclosure_or_approval() {
    use festerm_automation_workflow::review::Draft;

    let (mut coordinator, app, id, _) = fixture();
    let target = coordinator.targets[&id.chip_id()].reference;
    coordinator.stop_mcp();
    assert!(coordinator.settings(&[&app]).targets.is_empty());
    assert!(coordinator.broker.is_some());
    assert!(coordinator.peers.is_empty());
    assert!(coordinator.server.is_none());

    let selection = WorkflowView::captured_for_test(workflow_review_bundle());
    let mut review = workflow_review::State::default();
    review.reconcile(
        &selection,
        &[&app],
        &coordinator.targets,
        &coordinator.privacy_gate,
        true,
        false,
    );
    let view = review.view(&selection);
    assert_eq!(view.live_targets()[0].reference, target);
    let revision = selection.review_token().unwrap().0;
    let request = WorkflowReviewRequest::new(
        &selection,
        &revision,
        Draft {
            parameters: BTreeMap::new(),
            targets: BTreeSet::from([target]),
        },
    )
    .unwrap();
    review.prepare(request, &selection);
    let view = review.view(&selection);
    let proposal = view.proposal().unwrap().review().invocation();
    assert_eq!(proposal.targets()["target-1"], target);
    assert_eq!(
        proposal.grant_request().scope.observe,
        BTreeSet::from([target])
    );
    assert!(proposal.grant_request().scope.control.is_empty());
    assert!(coordinator.peers.is_empty());
    coordinator.privacy_gate.close();
    assert!(view.proposal().is_none());
}

#[test]
fn workflow_review_settings_broadcast_invalidates_the_selection_revision() {
    use festerm_config::{Configuration, InterfaceSettings};

    let (mut app, _) = FesTermApp::automation_fixture();
    let selection = WorkflowView::captured_for_test(workflow_review_bundle());
    app.set_automation_settings(SettingsSnapshot {
        workflow: selection.clone(),
        ..SettingsSnapshot::default()
    });

    let revision = selection.review_token().unwrap().0;
    let javascript = InterfaceSettings::DEFAULT.with_javascript_enabled(true);
    app.adopt_broadcast_configuration(
        Configuration::empty()
            .with_interface_settings(javascript.clone())
            .unwrap(),
    );
    assert!(!revision.is_current());

    let revision = selection.review_token().unwrap().0;
    app.adopt_broadcast_configuration(
        Configuration::empty()
            .with_interface_settings(javascript.with_mcp_enabled(true))
            .unwrap(),
    );
    assert!(!revision.is_current());

    app.adopt_broadcast_configuration(
        Configuration::empty()
            .with_interface_settings(InterfaceSettings::DEFAULT.with_mcp_enabled(true))
            .unwrap(),
    );
    assert!(selection.current_bundle().is_none());
}

#[test]
fn workflow_review_initialization_without_selection_starts_nothing() {
    let mut coordinator = Coordinator::default();
    coordinator.javascript_enabled = true;
    coordinator.command(
        NativeCommand::InitializeWorkflowReview,
        &egui::Context::default(),
    );
    assert!(coordinator.broker.is_none());
    assert!(coordinator.privacy.is_none());
    assert!(coordinator.starting.is_none());
    assert!(coordinator.server.is_none());
    assert!(!coordinator.want_shared_start);
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
    coordinator.command(
        NativeCommand::CaptureWorkflow(SelectedWorkflow(path)),
        &context,
    );
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
