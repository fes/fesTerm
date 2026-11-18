#![cfg(feature = "javascript-program-dev")]

use festerm_automation_js::WorkerProgram;
use festerm_javascript_session::{JavaScriptSession, ProcessJavaScriptRuntime};
use festerm_session::{
    Session, SessionEvent, SessionLifecycle, SessionTryReceiveError, ShutdownResult, TerminalSize,
};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn javascript_program_worker_entry_refuses_extra_arguments_exactly() {
    let output = Command::new(env!("CARGO_BIN_EXE_festerm"))
        .args(["--javascript-program-worker", "unexpected"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(64));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn javascript_program_process_session_preserves_input_resize_and_close() {
    if festerm_automation_js::platform::availability().is_err() {
        return;
    }
    let executable = Path::new(env!("CARGO_BIN_EXE_festerm"))
        .canonicalize()
        .unwrap();
    let worker = WorkerProgram::from_install_owned_absolute(&executable).unwrap();
    let session = JavaScriptSession::start(
        ProcessJavaScriptRuntime::developer_echo(worker).unwrap(),
        TerminalSize::new(80, 24).unwrap(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while session.lifecycle() != SessionLifecycle::Running {
        assert!(Instant::now() < deadline, "process session did not start");
        let _ = session.try_recv_event();
        std::thread::yield_now();
    }
    session.try_send_input(b"hello").unwrap();
    session
        .try_resize(TerminalSize::new(100, 30).unwrap())
        .unwrap();
    let mut output = Vec::new();
    while !output
        .windows(b"resize:100x30".len())
        .any(|window| window == b"resize:100x30")
    {
        assert!(Instant::now() < deadline, "process session output stalled");
        match session.try_recv_event() {
            Ok(SessionEvent::Output(bytes)) => output.extend_from_slice(&bytes),
            Ok(_) => {}
            Err(SessionTryReceiveError::Empty) => std::thread::yield_now(),
            Err(SessionTryReceiveError::Closed) => panic!("process session closed early"),
        }
    }
    assert!(output.windows(5).any(|window| window == b"hello"));
    assert_eq!(
        session.shutdown(Duration::from_secs(2)),
        Ok(ShutdownResult::Stopped)
    );
    let mut close_output = false;
    loop {
        match session.try_recv_event() {
            Ok(SessionEvent::Output(bytes)) if bytes == b"closed" => close_output = true,
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Stopped)) => {
                assert!(close_output);
                break;
            }
            Ok(_) => {}
            Err(SessionTryReceiveError::Empty | SessionTryReceiveError::Closed) => {
                panic!("process session missed terminal close output")
            }
        }
    }
}
