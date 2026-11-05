//! Synthetic controller approvals for the separate-process SDK fixture.

use super::{ControlReader, ControlWriter};
use crate::{
    app::FesTermApp,
    application::WindowId,
    automation::{
        self, controller as host, ipc, presentation::DirectActionDisplay, ControllerInputView,
        ControllerMetadataView, ControllerRow, Coordinator, NativeCommand,
    },
    session_controller::fake::FakeSshSession,
    tabs::{ApplicationSession, TabId},
};
use festerm_automation::ControllerRegistrationId;
use rmcp::{model::CallToolRequestParams, service::Peer, RoleClient};
use serde_json::{json, Value};
use std::{
    sync::{mpsc::SyncSender, Arc},
    time::{Duration, Instant},
};
use tokio::io::AsyncWriteExt;

const CHANNEL: u64 = 1;

pub(super) struct HostState {
    first_tab: TabId,
    second_tab: TabId,
    first_backend: FakeSshSession,
    second_backend: FakeSshSession,
    _ime: Arc<festerm_session::AutomationEpoch>,
}

pub(super) fn fixture() -> (FesTermApp, Coordinator, SyncSender<ipc::Event>, HostState) {
    let (mut app, first_tab) = FesTermApp::automation_local_fixture();
    let (mut donor, second_tab) = FesTermApp::automation_local_fixture();
    app.adopt_tab(donor.detach_tab(second_tab).unwrap(), None);
    app.automation_session_mut(first_tab).label = "same label".to_owned();
    app.automation_session_mut(second_tab).label = "same label".to_owned();
    let settings = festerm_config::InterfaceSettings::DEFAULT.with_mcp_enabled(true);
    app.adopt_broadcast_configuration(
        festerm_config::Configuration::empty()
            .with_interface_settings(settings)
            .unwrap(),
    );
    let ime = Arc::new(festerm_session::AutomationEpoch::new());
    ime.transition(true);
    app.set_automation_ime_epoch(&ime);
    let backend = |tab| {
        let Some(ApplicationSession::TestLocal(backend)) =
            app.automation_session(tab).unwrap().controller.session()
        else {
            panic!("expected synthetic local backend");
        };
        backend.clone()
    };
    let first_backend = backend(first_tab);
    let second_backend = backend(second_tab);
    first_backend.enable_ordered_test_input();
    second_backend.enable_ordered_test_input();
    let mut coordinator = host::tests::fixture(&[&app]);
    coordinator.synthetic_enrollment_metadata_ui_qualified = true;
    coordinator.synthetic_enrollment_metadata_delivery_qualified = true;
    coordinator.synthetic_direct_input_qualified = true;
    let (server, events) = ipc::test_server(coordinator.stop.clone());
    coordinator.server = Some(server);
    (
        app,
        coordinator,
        events,
        HostState {
            first_tab,
            second_tab,
            first_backend,
            second_backend,
            _ime: ime,
        },
    )
}

fn row(coordinator: &Coordinator, app: &FesTermApp, public_id: &str) -> ControllerRow {
    let snapshot = host::tests::stable_settings(coordinator, &[app]);
    snapshot
        .controllers
        .into_iter()
        .find(|row| row.channel == CHANNEL && row.public_id.to_wire() == public_id)
        .expect("client registration is visible")
}

fn approve_enrollment(
    coordinator: &mut Coordinator,
    app: &FesTermApp,
    registration: ControllerRegistrationId,
    tab: TabId,
) {
    let target = coordinator.targets[&tab.chip_id()].reference;
    let token = host::tests::review(
        coordinator,
        &[app],
        WindowId::PRIMARY,
        CHANNEL,
        registration,
        target,
    );
    host::tests::confirm(coordinator, &[app], WindowId::PRIMARY, registration, token);
}

fn approve_metadata(coordinator: &mut Coordinator, app: &FesTermApp, row: &ControllerRow) {
    let ControllerMetadataView::Idle {
        enrollment,
        granted: false,
        ..
    } = row.metadata
    else {
        panic!("metadata requires a separate approval");
    };
    host::tests::metadata_command_with_retry(
        coordinator,
        app,
        WindowId::PRIMARY,
        NativeCommand::ReviewControllerMetadata {
            channel: CHANNEL,
            registration: row.registration,
            enrollment,
        },
    )
    .unwrap();
    let token = host::tests::metadata_review_token(coordinator, app, row.registration);
    host::tests::metadata_command_with_retry(
        coordinator,
        app,
        WindowId::PRIMARY,
        NativeCommand::ConfirmControllerMetadata {
            channel: CHANNEL,
            registration: row.registration,
            review: token,
        },
    )
    .unwrap();
}

fn approve_and_dispatch_input(
    coordinator: &mut Coordinator,
    app: &mut FesTermApp,
    state: &HostState,
    row: &ControllerRow,
    text: &str,
    unknown: bool,
) -> u64 {
    assert_eq!(
        row.associated_target,
        Some(coordinator.targets[&state.second_tab.chip_id()].reference)
    );
    let ControllerInputView::Requested { token, .. } = row.input else {
        panic!("expected an explicitly requested input");
    };
    host::tests::native(
        coordinator,
        &[app],
        WindowId::PRIMARY,
        NativeCommand::ReviewControllerInput {
            channel: CHANNEL,
            registration: row.registration,
            input: token,
        },
    );
    let preparations = host::direct_input_preparations(coordinator);
    assert_eq!(preparations.len(), 1);
    automation::retry_automation_busy(|| {
        host::prepare_direct_input_review(
            coordinator,
            preparations[0],
            app.automation_session_mut(state.second_tab),
        )
    })
    .unwrap();
    let reviewed = host::tests::registration_row(coordinator, &[app], CHANNEL, row.registration);
    let ControllerInputView::Reviewing {
        action,
        metadata_sources,
        ..
    } = reviewed.input
    else {
        panic!("expected an input review");
    };
    assert_eq!(
        action,
        DirectActionDisplay::Text {
            escaped: text.to_owned(),
            submit: true,
        }
    );
    assert_eq!(metadata_sources.len(), 1);
    assert!(state.first_backend.drain_ordered_test_input().is_empty());
    assert!(state.second_backend.drain_ordered_test_input().is_empty());
    host::tests::native(
        coordinator,
        &[app],
        WindowId::PRIMARY,
        NativeCommand::ConfirmControllerInput {
            channel: CHANNEL,
            registration: row.registration,
            input: token,
        },
    );
    coordinator.synthetic_direct_input_lost_after_commit = unknown;
    let dispatches = host::direct_input_dispatches(coordinator);
    assert_eq!(dispatches.len(), 1);
    automation::retry_automation_busy(|| {
        host::dispatch_direct_input(
            coordinator,
            dispatches[0],
            app.automation_session(state.second_tab).unwrap(),
            &[app],
        )
    })
    .unwrap();
    host::tests::settle_direct_input_for_test(coordinator, token, app);
    assert_eq!(
        state.second_backend.drain_ordered_test_input(),
        vec![format!("{text}\r").into_bytes()]
    );
    assert!(state.first_backend.drain_ordered_test_input().is_empty());
    token
}

pub(super) async fn host(
    coordinator: &mut Coordinator,
    app: &mut FesTermApp,
    state: HostState,
    lines: &mut ControlReader,
    write: &mut ControlWriter,
) {
    let context = eframe::egui::Context::default();
    let mut expected_input: Option<(String, String, bool)> = None;
    let mut armed = None;
    let mut enrollments = 0;
    let mut metadata_approvals = 0;
    let mut moved = false;
    let mut inputs = 0;
    loop {
        coordinator.tick(&context, &[app]);
        if let Some((public_id, text, unknown)) = &expected_input {
            let candidate = row(coordinator, app, public_id);
            if matches!(candidate.input, ControllerInputView::Requested { .. }) {
                let token = approve_and_dispatch_input(
                    coordinator,
                    app,
                    &state,
                    &candidate,
                    text,
                    *unknown,
                );
                if *unknown {
                    let current = host::tests::registration_row(
                        coordinator,
                        &[app],
                        CHANNEL,
                        candidate.registration,
                    );
                    assert!(current.control.is_none());
                    armed = None;
                } else {
                    armed = Some((candidate.registration, token));
                }
                inputs += 1;
                expected_input = None;
            }
        }
        tokio::select! {
            line = lines.next_line() => {
                let line = line.unwrap().expect("controller test command");
                let fields: Vec<_> = line.split_ascii_whitespace().collect();
                match fields.as_slice() {
                    ["ENROLL", public_id] => {
                        let registration = row(coordinator, app, public_id).registration;
                        let tab = match enrollments {
                            0 => state.first_tab,
                            1 => state.second_tab,
                            _ => panic!("unexpected enrollment"),
                        };
                        approve_enrollment(coordinator, app, registration, tab);
                        enrollments += 1;
                        write.write_all(b"ACK\n").await.unwrap();
                    }
                    ["METADATA", public_id] => {
                        let candidate = row(coordinator, app, public_id);
                        approve_metadata(coordinator, app, &candidate);
                        metadata_approvals += 1;
                        write.write_all(b"ACK\n").await.unwrap();
                    }
                    ["MOVE_FIRST"] => {
                        coordinator.controller_target_moving(state.first_tab);
                        let deadline = Instant::now() + Duration::from_secs(5);
                        loop {
                            coordinator.tick(&context, &[app]);
                            if !coordinator.reconciliation_deferred
                                && coordinator.enrollment_metadata_grants.len() == 1
                            {
                                break;
                            }
                            assert!(Instant::now() < deadline, "target cleanup stayed busy");
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                        moved = true;
                        write.write_all(b"ACK\n").await.unwrap();
                    }
                    ["INPUT", public_id, text, disposition] => {
                        assert!(expected_input.is_none());
                        let unknown = match *disposition {
                            "admitted" => false,
                            "unknown" => true,
                            _ => panic!("unexpected disposition"),
                        };
                        expected_input = Some(((*public_id).to_owned(), (*text).to_owned(), unknown));
                        write.write_all(b"ACK\n").await.unwrap();
                    }
                    ["DISARM"] => {
                        let (registration, token) = armed.take().expect("input was armed");
                        host::tests::native(
                            coordinator,
                            &[app],
                            WindowId::PRIMARY,
                            NativeCommand::DisarmControllerInput {
                                channel: CHANNEL,
                                registration,
                                input: token,
                            },
                        );
                        let principal = coordinator.peers[&CHANNEL].principal;
                        let target = coordinator.targets[&state.second_tab.chip_id()].reference;
                        let deadline = Instant::now() + Duration::from_secs(5);
                        loop {
                            let mut broker = coordinator.broker.take().unwrap();
                            coordinator.reconcile(&mut broker, &[app]);
                            let disarmed = broker
                                .native_control_grant(principal, target, Instant::now())
                                .is_err();
                            coordinator.broker = Some(broker);
                            if disarmed {
                                break;
                            }
                            assert!(Instant::now() < deadline, "input disarm stayed busy");
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                        write.write_all(b"ACK\n").await.unwrap();
                    }
                    ["DONE"] => break,
                    other => panic!("unexpected controller test command: {other:?}"),
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(1)) => {}
        }
    }
    assert_eq!(enrollments, 2);
    assert_eq!(metadata_approvals, 2);
    assert!(moved);
    assert_eq!(inputs, 2);
    assert!(expected_input.is_none());
    assert!(armed.is_none());
    assert!(coordinator.peers[&CHANNEL].grant.is_none());
    assert!(!coordinator.javascript_enabled);
    assert!(!app.automation_javascript_enabled());
    assert!(state.first_backend.drain_ordered_test_input().is_empty());
    assert!(state.second_backend.drain_ordered_test_input().is_empty());
    write.write_all(b"FINISHED\n").await.unwrap();
}

async fn call(
    client: &Peer<RoleClient>,
    name: &'static str,
    arguments: Value,
) -> rmcp::model::CallToolResult {
    client
        .call_tool(
            CallToolRequestParams::new(name).with_arguments(arguments.as_object().unwrap().clone()),
        )
        .await
        .unwrap()
}

async fn command(lines: &mut ControlReader, write: &mut ControlWriter, value: &str) {
    write
        .write_all(format!("{value}\n").as_bytes())
        .await
        .unwrap();
    assert_eq!(lines.next_line().await.unwrap().as_deref(), Some("ACK"));
}

pub(super) async fn client(
    client: &Peer<RoleClient>,
    lines: &mut ControlReader,
    write: &mut ControlWriter,
) {
    let tools = client.list_all_tools().await.unwrap();
    for name in [
        "festerm_controller_capabilities",
        "festerm_controller_register",
        "festerm_controller_status",
        "festerm_sessions_get",
        "festerm_terminal_send_text",
    ] {
        assert!(tools.iter().any(|tool| tool.name == name), "{name}");
    }
    let capabilities = call(client, "festerm_controller_capabilities", json!({}))
        .await
        .structured_content
        .unwrap();
    let data = &capabilities["data"];
    assert_eq!(data["enrollment_available"], true);
    assert_eq!(data["metadata_available"], true);
    assert_eq!(data["metadata_requires_separate_grant"], true);
    assert_eq!(data["presentation_available"], false);
    assert_eq!(data["input_available"], true);
    let observation = call(client, "festerm_sessions_list", json!({})).await;
    assert_eq!(observation.is_error, Some(true));

    let mut registrations = Vec::new();
    let mut handles = Vec::new();
    for local_id in ["device-left", "device-right"] {
        let registered = call(
            client,
            "festerm_controller_register",
            json!({"local_id": local_id}),
        )
        .await
        .structured_content
        .unwrap();
        assert!(registered["data"]["association_id"].is_null());
        let registration = registered["data"]["registration_id"]
            .as_str()
            .unwrap()
            .to_owned();
        command(lines, write, &format!("ENROLL {registration}")).await;
        let status = call(
            client,
            "festerm_controller_status",
            json!({"registration_id": registration}),
        )
        .await
        .structured_content
        .unwrap();
        handles.push(
            status["data"]["association_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
        registrations.push(registration);
    }
    assert_ne!(handles[0], handles[1]);
    for (registration, handle) in registrations.iter().zip(&handles) {
        let denied = call(
            client,
            "festerm_sessions_get",
            json!({"session_ref": handle}),
        )
        .await;
        assert_eq!(denied.is_error, Some(true));
        assert_eq!(denied.structured_content.unwrap()["code"], "NOT_AUTHORIZED");
        command(lines, write, &format!("METADATA {registration}")).await;
        let metadata = call(
            client,
            "festerm_sessions_get",
            json!({"session_ref": handle}),
        )
        .await
        .structured_content
        .unwrap();
        assert_eq!(metadata["status"], "ok");
        assert_eq!(
            metadata["data"]["sessions"][0]["session_ref"],
            handle.as_str()
        );
        assert_eq!(metadata["data"]["sessions"][0]["label"], "same label");
        assert_eq!(metadata["data"]["sessions"][0]["lifecycle"], "running");
    }
    command(lines, write, "MOVE_FIRST").await;
    let stale = call(
        client,
        "festerm_sessions_get",
        json!({"session_ref": &handles[0]}),
    )
    .await;
    assert_eq!(stale.is_error, Some(true));
    assert_eq!(stale.structured_content.unwrap()["code"], "NOT_AUTHORIZED");
    let current = call(
        client,
        "festerm_sessions_get",
        json!({"session_ref": &handles[1]}),
    )
    .await
    .structured_content
    .unwrap();
    assert_eq!(current["data"]["sessions"][0]["session_ref"], handles[1]);
    let stale_input = call(
        client,
        "festerm_terminal_send_text",
        json!({"enrollment_ref": &handles[0], "text": "never sent"}),
    )
    .await;
    assert_eq!(stale_input.is_error, Some(true));
    assert_eq!(
        stale_input.structured_content.unwrap()["code"],
        "NOT_AUTHORIZED"
    );
    let no_presentation = call(
        client,
        "festerm_terminal_send_text",
        json!({
            "enrollment_ref": &handles[1],
            "text": "not sent",
            "presentation_revision": "a".repeat(48),
        }),
    )
    .await;
    assert_eq!(no_presentation.is_error, Some(true));
    assert_eq!(
        no_presentation.structured_content.unwrap()["code"],
        "UNAVAILABLE"
    );
    for (text, expected) in [("admitted", "admitted"), ("uncertain", "unknown")] {
        command(
            lines,
            write,
            &format!("INPUT {} {text} {expected}", registrations[1]),
        )
        .await;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let result = call(
                client,
                "festerm_terminal_send_text",
                json!({"enrollment_ref": &handles[1], "text": text, "submit": true}),
            )
            .await;
            let body = result.structured_content.unwrap();
            if body["status"] == "refused" && body["code"] == "UNAVAILABLE" {
                assert!(Instant::now() < deadline, "SDK input stayed unavailable");
                tokio::time::sleep(Duration::from_millis(1)).await;
                continue;
            }
            assert_eq!(result.is_error, Some(expected == "unknown"));
            assert_eq!(
                body["data"]["disposition"],
                if expected == "unknown" {
                    "outcome_unknown"
                } else {
                    "admitted"
                }
            );
            break;
        }
        if expected == "admitted" {
            command(lines, write, "DISARM").await;
        }
    }
}
