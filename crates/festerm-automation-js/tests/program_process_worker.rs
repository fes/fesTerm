use festerm_automation_js::{
    platform,
    program::{
        developer_echo_program, ApprovedProgram, ProgramCapability, ProgramHostMessage,
        ProgramKind, ProgramManifest, ProgramSourceSnapshot, ProgramWorkerMessage, Submission,
        TerminalSize,
    },
    protocol::read_frame,
    ProgramSupervisor, WorkerProgram,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn worker() -> WorkerProgram {
    let path = Path::new(env!("CARGO_BIN_EXE_festerm-automation-js-worker"))
        .canonicalize()
        .unwrap();
    WorkerProgram::from_install_owned_absolute(&path).unwrap()
}

fn initial_size() -> TerminalSize {
    TerminalSize {
        columns: 80,
        rows: 24,
    }
}

#[test]
fn program_worker_is_ready_before_source_is_sent() {
    if platform::availability().is_err() {
        return;
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_festerm-automation-js-worker"));
    command
        .arg("--javascript-program-worker")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(target_os = "linux")]
    command.env("MALLOC_ARENA_MAX", "1");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let message = read_frame::<_, ProgramWorkerMessage>(&mut stdout).unwrap();
    assert!(matches!(
        message,
        ProgramWorkerMessage::Ready { ref profile }
            if profile == platform::availability().unwrap()
    ));
    // No Begin/source frame has been sent. The worker must remain confined and
    // waiting rather than constructing or executing a source snapshot.
    std::thread::sleep(Duration::from_millis(20));
    assert!(child.try_wait().unwrap().is_none());
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn confined_program_worker_echoes_normal_output_and_closes() {
    if platform::availability().is_err() {
        return;
    }
    let mut program = ProgramSupervisor::spawn(
        &worker(),
        developer_echo_program().unwrap(),
        7,
        initial_size(),
    )
    .unwrap();
    assert_eq!(program.input(b"hello").unwrap(), vec![b"hello".to_vec()]);
    assert!(program.input(&[0xc3]).unwrap().is_empty());
    assert_eq!(
        program.input(&[0xa9]).unwrap(),
        vec!["é".as_bytes().to_vec()]
    );
    assert_eq!(
        program
            .resize(TerminalSize {
                columns: 100,
                rows: 30,
            })
            .unwrap(),
        vec![b"resize:100x30".to_vec()]
    );
    assert_eq!(program.close().unwrap(), vec![b"closed".to_vec()]);
    program.shutdown().unwrap();
}

#[test]
fn confined_program_worker_refuses_stale_generation_without_advancing() {
    if platform::availability().is_err() {
        return;
    }
    let mut program = ProgramSupervisor::spawn(
        &worker(),
        developer_echo_program().unwrap(),
        7,
        initial_size(),
    )
    .unwrap();
    let (submission, output) = program
        .submit(&ProgramHostMessage::Input {
            generation: 6,
            sequence: 1,
            bytes: b"stale".to_vec(),
        })
        .unwrap();
    assert_eq!(submission, Submission::StaleGeneration);
    assert!(output.is_empty());
    assert_eq!(program.input(b"live").unwrap(), vec![b"live".to_vec()]);
    program.close().unwrap();
    program.shutdown().unwrap();
}

#[test]
fn confined_program_worker_close_is_force_bounded() {
    if platform::availability().is_err() {
        return;
    }
    let source = ProgramSourceSnapshot {
        manifest: ProgramManifest {
            version: 1,
            api_version: 1,
            kind: ProgramKind::TerminalProgram,
            name: "hostile-close".into(),
            entry: "main.js".into(),
            modules: vec!["main.js".into()],
            capabilities: BTreeSet::from([
                ProgramCapability::TerminalInput,
                ProgramCapability::TerminalOutput,
                ProgramCapability::TerminalResize,
            ]),
        },
        modules: BTreeMap::from([(
            "main.js".into(),
            r#"export function main({terminal}) {
                terminal.events(event => { if (event.type === "close") while (true) {} });
            }"#
            .into(),
        )]),
    };
    let mut program = ProgramSupervisor::spawn(
        &worker(),
        ApprovedProgram::new(source).unwrap(),
        9,
        initial_size(),
    )
    .unwrap();
    let started = Instant::now();
    assert!(program.close().is_err());
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(
        std::thread::spawn(move || program.shutdown())
            .join()
            .unwrap()
            .is_ok(),
        "forced cleanup must still reap the owned child"
    );
}

#[test]
fn javascript_program_worker_refuses_extra_arguments_exactly() {
    let output = Command::new(env!("CARGO_BIN_EXE_festerm-automation-js-worker"))
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
