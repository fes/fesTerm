//! Separate-process interoperability with test-only comparison and approval.

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
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
};

const CHILD_SOCKET: &str = "FESTERM_DIRECT_SDK_PROCESS_TEST_SOCKET";
const TEST_NAME: &str =
    "automation::ipc::direct::process_tests::sdk_observation_crosses_process_boundary";
const TEST_TIMEOUT: Duration = Duration::from_secs(20);

#[tokio::test(flavor = "current_thread")]
async fn sdk_observation_crosses_process_boundary() {
    if let Some(path) = std::env::var_os(CHILD_SOCKET) {
        tokio::time::timeout(TEST_TIMEOUT, child_client(PathBuf::from(path)))
            .await
            .expect("client process timed out");
        return;
    }
    tokio::task::LocalSet::new()
        .run_until(parent_coordinator())
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

async fn parent_coordinator() {
    let (app, tab, mut coordinator, events) = tests::observation_fixture();
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
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD_SOCKET, &path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(output.try_clone().unwrap()))
        .stderr(Stdio::from(child_control))
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
    let native = Arc::new(tests::NativeState::default());
    let wake = Wake::default();
    let stop = coordinator.stop.clone();
    let owner = owner::Owner::new(
        host_socket,
        tests::Native(native.clone()),
        tests::Admission {
            open: Arc::new(AtomicBool::new(true)),
            cancelled: Arc::new(AtomicBool::new(false)),
            fail_cleanup: false,
        },
        PeerContext {
            bootstrap: QualifiedBootstrap::synthetic(),
            key: 17,
            events,
            wake_ui: Arc::new(|| {}),
            stop: stop.clone(),
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
    let exchange_wake = wake.clone();
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
        let host_code = loop {
            if let Some(code) = native.displayed.lock().unwrap().clone() {
                break code;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        };
        assert_eq!(client_code, host_code);
        write
            .write_all(format!("HOST {host_code}\n").as_bytes())
            .await
            .unwrap();
        *native.decision.lock().unwrap() = Some(client_code.as_bytes().try_into().unwrap());
        exchange_wake.notify();

        let context = eframe::egui::Context::default();
        let mut approved = false;
        loop {
            coordinator.tick(&context, &[&app]);
            tokio::select! {
                line = lines.next_line() => {
                    match line.unwrap().as_deref() {
                        Some("APPROVE") if !approved => {
                            let target = coordinator.targets[&tab.chip_id()].reference;
                            super::super::super::tests::approve_for_test(
                                &mut coordinator, 17, vec![target],
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

async fn child_client(path: PathBuf) {
    let control_fd = std::io::stderr().as_fd().try_clone_to_owned().unwrap();
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
    write.write_all(b"DONE\n").await.unwrap();
    client.cancel().await.unwrap();
    assert_eq!(status.wait().await, Ok(()));
}
