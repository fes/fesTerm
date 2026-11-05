//! Separate-process interoperability with simulated human comparison and approval.

mod controller;

use super::{owner, secure_transport, tests, Error, PeerContext, QualifiedBootstrap, Wake};
use festerm_automation_mcp::secure_transport::MAX_FRAME_DURATION;
use festerm_automation_mcp_client::DirectClientTransport;
use rmcp::{
    model::{CallToolRequestParams, ClientConfig, ProtocolVersion},
    ServiceExt,
};
use serde_json::json;
use std::{
    io::{Read, Seek},
    os::{
        fd::{AsFd, OwnedFd},
        unix::net::UnixStream as StdUnixStream,
    },
    path::PathBuf,
    process::Stdio,
    sync::{atomic::AtomicBool, mpsc, Arc},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
};

const CHILD_SOCKET: &str = "FESTERM_DIRECT_SDK_PROCESS_TEST_SOCKET";
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

type ControlReader = tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>;
type ControlWriter = tokio::net::unix::OwnedWriteHalf;

#[derive(Clone, Copy)]
enum Scenario {
    Observation,
    Controller,
}

impl Scenario {
    fn test_name(self) -> &'static str {
        match self {
            Self::Observation => {
                "automation::ipc::direct::process_tests::sdk_observation_crosses_process_boundary"
            }
            Self::Controller => {
                "automation::ipc::direct::process_tests::sdk_controller_crosses_process_boundary"
            }
        }
    }

    fn channel(self) -> u64 {
        match self {
            Self::Observation => 17,
            Self::Controller => 1,
        }
    }
}

enum HostScenario {
    Observation(crate::tabs::TabId),
    Controller(controller::HostState),
}

#[tokio::test(flavor = "current_thread")]
async fn sdk_observation_crosses_process_boundary() {
    run_scenario(Scenario::Observation).await;
}

#[tokio::test(flavor = "current_thread")]
async fn sdk_controller_crosses_process_boundary() {
    run_scenario(Scenario::Controller).await;
}

async fn run_scenario(scenario: Scenario) {
    if let Some(path) = std::env::var_os(CHILD_SOCKET) {
        tokio::time::timeout(TEST_TIMEOUT, child_client(PathBuf::from(path), scenario))
            .await
            .expect("client process timed out");
        return;
    }
    tokio::task::LocalSet::new()
        .run_until(parent_coordinator(scenario))
        .await;
}

async fn terminate_child(child: &mut tokio::process::Child) {
    if child.try_wait().expect("check SDK client").is_none() {
        let termination = child.start_kill();
        assert!(
            termination.is_ok() || matches!(child.try_wait(), Ok(Some(_))),
            "could not terminate SDK client: {termination:?}"
        );
    }
    tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("SDK client did not exit after termination")
        .expect("reap terminated SDK client");
}

#[tokio::test(flavor = "current_thread")]
async fn stalled_client_is_terminated_and_reaped() {
    let mut child = tokio::process::Command::new("/bin/sleep")
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn stalled client");
    terminate_child(&mut child).await;
    assert!(child.try_wait().expect("check reaped client").is_some());
}

async fn parent_coordinator(scenario: Scenario) {
    let (app, mut coordinator, events, host_scenario) = match scenario {
        Scenario::Observation => {
            let (app, tab, coordinator, events) = tests::observation_fixture();
            (app, coordinator, events, HostScenario::Observation(tab))
        }
        Scenario::Controller => {
            let (app, coordinator, events, state) = controller::fixture();
            (app, coordinator, events, HostScenario::Controller(state))
        }
    };
    let directory = tempfile::Builder::new()
        .prefix("mcp-process-")
        .tempdir_in("/tmp")
        .unwrap();
    let path = directory.path().join("peer.sock");
    let listener = UnixListener::bind(&path).expect("bind test-only Unix listener");
    let (parent_control, child_control) = StdUnixStream::pair().unwrap();
    parent_control.set_nonblocking(true).unwrap();
    let control = UnixStream::from_std(parent_control).unwrap();
    let child_control: OwnedFd = child_control.into();
    let mut output = tempfile::tempfile().unwrap();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", scenario.test_name(), "--nocapture"])
        .env(CHILD_SOCKET, &path)
        .stdin(Stdio::from(child_control))
        .stdout(Stdio::from(output.try_clone().unwrap()))
        .stderr(Stdio::from(output.try_clone().unwrap()))
        .kill_on_drop(true)
        .spawn()
        .expect("spawn SDK client process");
    let client_pid = child.id().expect("SDK client has a PID");
    assert_ne!(client_pid, std::process::id());

    let (host_socket, _) = match tokio::time::timeout(TEST_TIMEOUT, listener.accept()).await {
        Ok(Ok(peer)) => peer,
        result => {
            terminate_child(&mut child).await;
            panic!("SDK client did not connect: {result:?}");
        }
    };
    let wake = Wake::default();
    let ui_wake = Arc::new(tokio::sync::Notify::new());
    let wake_ui = {
        let ui_wake = ui_wake.clone();
        Arc::new(move || ui_wake.notify_one())
    };
    let wake_owner = {
        let wake = wake.clone();
        Arc::new(move || wake.notify())
    };
    let surface = coordinator.direct_verification_surface();
    let deadline = Instant::now() + Duration::from_secs(5);
    let native = loop {
        match surface.reserve(
            scenario.channel(),
            crate::application::WindowId::PRIMARY,
            coordinator.stop.clone(),
            coordinator.privacy_gate.clone(),
            wake_ui.clone(),
            wake_owner.clone(),
        ) {
            Ok(native) => break native,
            Err(super::super::super::verification::Error::Busy) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            Err(error) => {
                terminate_child(&mut child).await;
                panic!("could not reserve native comparison: {error:?}");
            }
        }
    };
    let stop = coordinator.stop.clone();
    let owner = owner::Owner::new(
        host_socket,
        native,
        tests::Admission {
            open: Arc::new(AtomicBool::new(true)),
            cancelled: Arc::new(AtomicBool::new(false)),
            fail_cleanup: false,
        },
        PeerContext {
            bootstrap: QualifiedBootstrap::synthetic(),
            key: scenario.channel(),
            events,
            wake_ui: Arc::new(|| {}),
            stop: stop.clone(),
            privacy_closed: coordinator.privacy_gate.closed_flag(),
            wake: wake.clone(),
        },
        Duration::from_secs(8),
        MAX_FRAME_DURATION,
    );
    let (host, mut host_status) = match owner {
        Ok(owner) => owner,
        Err(error) => {
            terminate_child(&mut child).await;
            panic!("could not create host owner: {error:?}");
        }
    };
    let host_task = tokio::spawn(host.run());
    let (read, mut write) = control.into_split();
    let mut lines = BufReader::new(read).lines();
    let mut exchange_task = tokio::task::spawn_local(async move {
        let ready = lines
            .next_line()
            .await
            .unwrap()
            .expect("client fingerprint");
        let mut fields = ready.split_ascii_whitespace();
        assert_eq!(fields.next(), Some("CLIENT"));
        assert_eq!(fields.next().unwrap().parse::<u32>().unwrap(), client_pid);
        let client_code = fields.next().expect("client comparison code");
        assert!(fields.next().is_none());
        let context = eframe::egui::Context::default();
        let mut app = app;
        let view = loop {
            coordinator.tick(&context, &[&app]);
            if let Some(view) = coordinator.settings(&[&app]).direct_verification {
                break view;
            }
            tokio::select! {
                _ = ui_wake.notified() => {}
                _ = tokio::time::sleep(Duration::from_millis(5)) => {}
            }
        };
        assert_eq!(view.channel, scenario.channel());
        assert_eq!(view.window, crate::application::WindowId::PRIMARY);
        assert_eq!(client_code, view.fingerprint);
        write
            .write_all(format!("HOST {}\n", view.fingerprint).as_bytes())
            .await
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            coordinator.commands(
                vec![super::super::super::NativeWindowCommand {
                    window: view.window,
                    command: super::super::super::NativeCommand::CompareDirectPeer {
                        generation: view.generation,
                        fingerprint: client_code.to_owned(),
                    },
                }],
                &context,
                &[&app],
            );
            if !view.is_current() {
                assert_eq!(
                    coordinator.status,
                    "Direct MCP comparison submitted; no terminal access was granted."
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "native comparison stayed busy or unavailable"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }

        match host_scenario {
            HostScenario::Observation(tab) => {
                observation_host(&mut coordinator, &app, tab, &mut lines, &mut write).await;
            }
            HostScenario::Controller(state) => {
                controller::host(&mut coordinator, &mut app, state, &mut lines, &mut write).await;
            }
        }
        (coordinator, app)
    });
    let exchange = tokio::time::timeout(TEST_TIMEOUT, &mut exchange_task).await;
    if !matches!(&exchange, Ok(Ok(_))) {
        if exchange.is_err() {
            exchange_task.abort();
        }
        terminate_child(&mut child).await;
        super::super::super::set_admission_flag(&stop, true);
        wake.notify();
        let result = tokio::time::timeout(Duration::from_secs(5), host_task)
            .await
            .expect("host task did not stop after timeout")
            .expect("host task panicked after timeout");
        assert_eq!(host_status.wait().await, result);
        if exchange.is_err() {
            let cancelled = match tokio::time::timeout(Duration::from_secs(5), exchange_task)
                .await
                .expect("exchange task did not stop after cancellation")
            {
                Err(error) => error,
                Ok(_) => panic!("timed-out exchange unexpectedly completed"),
            };
            assert!(
                cancelled.is_cancelled(),
                "exchange task panicked: {cancelled}"
            );
        }
        output.rewind().unwrap();
        let mut child_output = String::new();
        output.read_to_string(&mut child_output).unwrap();
        eprintln!("SDK child output:\n{child_output}");
        match exchange {
            Ok(Err(error)) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Ok(Err(error)) => panic!("exchange task was interrupted: {error}"),
            Err(_) => panic!("separate-process exchange timed out"),
            Ok(Ok(_)) => unreachable!(),
        }
    }
    let (_coordinator, _app) = exchange
        .expect("exchange completed before the deadline")
        .expect("exchange task completed");
    let child_status = match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(status)) => status,
        result => {
            terminate_child(&mut child).await;
            panic!("SDK client did not exit: {result:?}");
        }
    };
    output.rewind().unwrap();
    let mut child_output = String::new();
    output.read_to_string(&mut child_output).unwrap();
    assert!(child_status.success(), "SDK child failed: {child_output}");
    assert!(child_output.contains("running 1 test"));
    super::super::super::set_admission_flag(&stop, true);
    wake.notify();
    let result = tokio::time::timeout(Duration::from_secs(5), host_task)
        .await
        .expect("host task did not stop")
        .expect("host task did not panic");
    assert!(matches!(
        result,
        Ok(()) | Err(Error::Channel(secure_transport::Error::PeerClosed))
    ));
    assert_eq!(host_status.wait().await, result);
}

async fn observation_host(
    coordinator: &mut super::super::super::Coordinator,
    app: &super::super::super::FesTermApp,
    tab: crate::tabs::TabId,
    lines: &mut ControlReader,
    write: &mut ControlWriter,
) {
    let context = eframe::egui::Context::default();
    let mut approved = false;
    loop {
        coordinator.tick(&context, &[app]);
        tokio::select! {
            line = lines.next_line() => {
                match line.unwrap().as_deref() {
                    Some("APPROVE") if !approved => {
                        let target = coordinator.targets[&tab.chip_id()].reference;
                        super::super::super::tests::approve_for_test(
                            coordinator, Scenario::Observation.channel(), vec![target],
                        ).unwrap();
                        approved = true;
                        write.write_all(b"APPROVED\n").await.unwrap();
                    }
                    Some("DONE") if approved => break,
                    other => panic!("unexpected test control message: {other:?}"),
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(1)) => {}
        }
    }
    write.write_all(b"FINISHED\n").await.unwrap();
}

async fn child_client(path: PathBuf, scenario: Scenario) {
    let control_fd = std::io::stdin().as_fd().try_clone_to_owned().unwrap();
    let control = StdUnixStream::from(control_fd);
    control.set_nonblocking(true).unwrap();
    let control = UnixStream::from_std(control).unwrap();
    let (read, mut write) = control.into_split();
    let mut lines = BufReader::new(read).lines();

    let socket = UnixStream::connect(&path).await.unwrap();
    let native = Arc::new(tests::NativeState::default());
    let (wake_tx, wake_rx) = mpsc::sync_channel(1);
    let native_for_driver = native.clone();
    let (transport, mut status) = DirectClientTransport::new(
        socket,
        Duration::from_secs(8),
        MAX_FRAME_DURATION,
        move |wake| {
            wake_tx.send(wake).unwrap();
            (tests::Native(native_for_driver), tests::ClientAdmission)
        },
    )
    .unwrap();
    let wake = wake_rx.recv().unwrap();
    let client_code = loop {
        if let Some(code) = native.displayed.lock().unwrap().clone() {
            break code;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    };
    write
        .write_all(format!("CLIENT {} {client_code}\n", std::process::id()).as_bytes())
        .await
        .unwrap();
    let host = lines.next_line().await.unwrap().expect("host fingerprint");
    let host_code = host.strip_prefix("HOST ").expect("host comparison code");
    *native.decision.lock().unwrap() = Some(host_code.as_bytes().try_into().unwrap());
    wake.wake();

    let mut config = ClientConfig::default();
    config.protocol_version = ProtocolVersion::V_2025_11_25;
    let client = config.serve(transport).await.unwrap();
    match scenario {
        Scenario::Observation => observation_client(client.peer(), &mut lines, &mut write).await,
        Scenario::Controller => controller::client(client.peer(), &mut lines, &mut write).await,
    }
    write.write_all(b"DONE\n").await.unwrap();
    assert_eq!(
        lines.next_line().await.unwrap().as_deref(),
        Some("FINISHED")
    );
    client.cancel().await.unwrap();
    assert_eq!(status.wait().await, Ok(()));
}

async fn observation_client(
    client: &rmcp::service::Peer<rmcp::RoleClient>,
    lines: &mut ControlReader,
    write: &mut ControlWriter,
) {
    let tools = client.list_all_tools().await.unwrap();
    assert!(tools
        .iter()
        .any(|tool| tool.name == "festerm_sessions_list"));
    assert!(!tools
        .iter()
        .any(|tool| tool.name == "festerm_terminal_send_text"));
    let request = || {
        CallToolRequestParams::new("festerm_sessions_list").with_arguments(serde_json::Map::new())
    };
    let denied = client.call_tool(request()).await.unwrap();
    assert_eq!(denied.is_error, Some(true));
    assert_eq!(denied.structured_content.unwrap()["code"], "NOT_AUTHORIZED");
    write.write_all(b"APPROVE\n").await.unwrap();
    assert_eq!(
        lines.next_line().await.unwrap().as_deref(),
        Some("APPROVED")
    );
    let listed = client
        .call_tool(request())
        .await
        .unwrap()
        .structured_content
        .unwrap();
    let sessions = listed["data"]["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    let reference = sessions[0]["session_ref"].as_str().unwrap();
    let screen = client
        .call_tool(
            CallToolRequestParams::new("festerm_terminal_read_screen").with_arguments(
                json!({"session_ref": reference, "max_bytes": 4096})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert!(screen.structured_content.unwrap()["data"]["text"]
        .as_str()
        .unwrap()
        .starts_with("approved source"));
}
