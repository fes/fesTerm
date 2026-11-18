use festerm_automation_js::{
    platform,
    program::{
        developer_echo_program, developer_host_services_program, ApprovedProgram,
        ProgramCapability, ProgramHostMessage, ProgramKind, ProgramManifest, ProgramSourceSnapshot,
        Submission, TerminalSize,
    },
    Error, ProgramDeliveryFence, ProgramSupervisor, WorkerProgram,
};
#[cfg(not(windows))]
use festerm_automation_js::{program::ProgramWorkerMessage, protocol::read_frame};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

static PROGRAM_PROCESS_TEST: Mutex<()> = Mutex::new(());

fn serialize_process_test() -> std::sync::MutexGuard<'static, ()> {
    PROGRAM_PROCESS_TEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn worker() -> WorkerProgram {
    let path = Path::new(env!("CARGO_BIN_EXE_festerm-automation-js-worker"))
        .canonicalize()
        .unwrap();
    WorkerProgram::from_install_owned_absolute(&path).unwrap()
}

struct Fence {
    cancelled: Arc<AtomicBool>,
}

impl ProgramDeliveryFence for Fence {
    fn is_live(&self, worker_generation: u64) -> bool {
        worker_generation == 11 && !self.cancelled.load(Ordering::Acquire)
    }

    fn try_enter(
        &self,
        worker_generation: u64,
    ) -> Result<
        Option<Box<dyn festerm_automation_js::ProgramDeliveryPermit + '_>>,
        festerm_automation_js::Error,
    > {
        if worker_generation != 11 || self.cancelled.load(Ordering::Acquire) {
            Err(festerm_automation_js::Error::DisclosureRevoked)
        } else {
            Ok(Some(Box::new(())))
        }
    }
}

fn initial_size() -> TerminalSize {
    TerminalSize {
        columns: 80,
        rows: 24,
    }
}

#[test]
#[cfg(not(windows))]
fn program_worker_is_ready_before_source_is_sent() {
    let _serial = serialize_process_test();
    if platform::program_availability().is_err() {
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
            if profile == platform::program_availability().unwrap()
    ));
    // No Begin/source frame has been sent. The worker must remain confined and
    // waiting rather than constructing or executing a source snapshot.
    std::thread::sleep(Duration::from_millis(20));
    assert!(child.try_wait().unwrap().is_none());
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
#[cfg(not(windows))]
fn automation_and_program_native_profiles_are_separate() {
    let _serial = serialize_process_test();
    if platform::availability().is_err() || platform::program_availability().is_err() {
        return;
    }
    for (argument, expected) in [
        ("--automation-worker", platform::availability().unwrap()),
        (
            "--javascript-program-worker",
            platform::program_availability().unwrap(),
        ),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_festerm-automation-js-worker"));
        command
            .arg(argument)
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
        let message =
            read_frame::<_, ProgramWorkerMessage>(&mut child.stdout.take().unwrap()).unwrap();
        match message {
            ProgramWorkerMessage::Ready { profile } => assert_eq!(profile, expected),
            other if argument == "--automation-worker" => {
                panic!("unexpected automation worker frame: {other:?}")
            }
            other => panic!("unexpected program worker frame: {other:?}"),
        }
        child.kill().unwrap();
        child.wait().unwrap();
    }
}

#[test]
fn confined_program_worker_echoes_normal_output_and_closes() {
    let _serial = serialize_process_test();
    if platform::program_availability().is_err() {
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
    assert!(program.close().unwrap().is_empty());
    program.shutdown().unwrap();
}

#[test]
fn confined_program_worker_serves_assets_clock_and_async_stream() {
    if platform::program_availability().is_err() {
        return;
    }
    let mut program = ProgramSupervisor::spawn(
        &worker(),
        developer_host_services_program().unwrap(),
        7,
        initial_size(),
    )
    .unwrap();
    assert_eq!(program.take_start_output(), vec![b"asset:ready\n".to_vec()]);
    assert!(program.input(&[0xc3]).unwrap().is_empty());
    assert_eq!(
        program.input(&[0xa9]).unwrap(),
        vec!["é".as_bytes().to_vec()]
    );
    assert!(program.close().unwrap().is_empty());
    program.shutdown().unwrap();
}

#[test]
fn confined_program_worker_completes_a_long_bounded_sleep() {
    if platform::program_availability().is_err() {
        return;
    }
    let approved = developer_host_services_program().unwrap();
    let mut source = approved.snapshot().clone();
    source.modules.insert(
        "main.js".into(),
        r#"export async function main({terminal, clock}) {
    terminal.write("before");
    await clock.sleep(1100);
    terminal.write("after");
}"#
        .into(),
    );
    let started = Instant::now();
    let mut program = ProgramSupervisor::spawn(
        &worker(),
        ApprovedProgram::with_assets(source, approved.assets().clone()).unwrap(),
        7,
        initial_size(),
    )
    .unwrap();
    assert_eq!(
        program.take_start_output(),
        vec![b"before".to_vec(), b"after".to_vec()]
    );
    assert!(program.is_completed());
    assert!(started.elapsed() >= Duration::from_millis(1_000));
    program.shutdown().unwrap();
}

#[test]
fn confined_program_worker_cancels_a_long_pending_sleep() {
    if platform::program_availability().is_err() {
        return;
    }
    let approved = developer_host_services_program().unwrap();
    let mut source = approved.snapshot().clone();
    source.modules.insert(
        "main.js".into(),
        r#"export async function main({terminal, clock}) {
    const event = await terminal.events().next();
    if (event.value.type === "input") await clock.sleep(60000);
    terminal.write("late");
}"#
        .into(),
    );
    let forced = Arc::new(AtomicBool::new(false));
    let program = ProgramSupervisor::spawn_with_authority(
        &worker(),
        ApprovedProgram::with_assets(source, approved.assets().clone()).unwrap(),
        11,
        initial_size(),
        forced.clone(),
        Arc::new(Fence {
            cancelled: Arc::new(AtomicBool::new(false)),
        }),
    )
    .unwrap();
    let pending = std::thread::spawn(move || {
        let mut program = program;
        let result = program.input(b"x");
        (program, result)
    });
    std::thread::sleep(Duration::from_millis(50));
    forced.store(true, Ordering::Release);
    let (program, result) = pending.join().unwrap();
    assert!(result.is_err());
    assert!(program.shutdown().is_err());
}

#[test]
fn confined_program_worker_close_does_not_wait_for_clock_sleep() {
    if platform::program_availability().is_err() {
        return;
    }
    let approved = developer_host_services_program().unwrap();
    let mut source = approved.snapshot().clone();
    source.modules.insert(
        "main.js".into(),
        r#"export async function main({terminal, clock}) {
    const event = await terminal.events().next();
    if (event.value.type === "close") {
        try { await clock.sleep(60000); } catch (_) {}
        terminal.write("closed");
    }
}"#
        .into(),
    );
    let mut program = ProgramSupervisor::spawn(
        &worker(),
        ApprovedProgram::with_assets(source, approved.assets().clone()).unwrap(),
        7,
        initial_size(),
    )
    .unwrap();
    let started = Instant::now();
    assert_eq!(program.close().unwrap(), vec![b"closed".to_vec()]);
    assert!(started.elapsed() < Duration::from_millis(500));
    program.shutdown().unwrap();
}

#[test]
fn confined_program_worker_closes_admission_when_async_main_settles() {
    if platform::program_availability().is_err() {
        return;
    }
    let mut source = developer_echo_program().unwrap().snapshot().clone();
    source.modules.insert(
        "main.js".into(),
        "export async function main({terminal}) { terminal.write('done'); }".into(),
    );
    let mut program = ProgramSupervisor::spawn(
        &worker(),
        ApprovedProgram::new(source).unwrap(),
        7,
        initial_size(),
    )
    .unwrap();
    assert_eq!(program.take_start_output(), vec![b"done".to_vec()]);
    assert!(program.input(b"late").is_err());
    assert!(program.close().unwrap().is_empty());
    program.shutdown().unwrap();
}

#[test]
fn confined_program_worker_latches_async_main_completion_after_input() {
    if platform::program_availability().is_err() {
        return;
    }
    let mut source = developer_echo_program().unwrap().snapshot().clone();
    source.modules.insert(
        "main.js".into(),
        r#"export async function main({terminal}) {
    const event = await terminal.events().next();
    if (event.value.type === "input") terminal.write("done");
}"#
        .into(),
    );
    let mut program = ProgramSupervisor::spawn(
        &worker(),
        ApprovedProgram::new(source).unwrap(),
        7,
        initial_size(),
    )
    .unwrap();
    assert_eq!(program.input(b"x").unwrap(), vec![b"done".to_vec()]);
    assert!(program.close().unwrap().is_empty());
    assert!(program.close().unwrap().is_empty());
    program.shutdown().unwrap();
}

#[test]
fn confined_program_worker_idle_wait_survives_and_reuses_budget() {
    let _serial = serialize_process_test();
    if platform::program_availability().is_err() {
        return;
    }
    let mut program = ProgramSupervisor::spawn(
        &worker(),
        developer_echo_program().unwrap(),
        8,
        initial_size(),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(1_100));
    assert_eq!(
        program.input(b"still-live").unwrap(),
        vec![b"still-live".to_vec()]
    );
    program.close().unwrap();
    program.shutdown().unwrap();
}

#[test]
fn confined_program_worker_refuses_stale_generation_without_advancing() {
    let _serial = serialize_process_test();
    if platform::program_availability().is_err() {
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
    let _serial = serialize_process_test();
    if platform::program_availability().is_err() {
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
            assets: Vec::new(),
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
    let cleanup = std::thread::spawn(move || program.shutdown())
        .join()
        .expect("forced cleanup must still join the owned child");
    assert!(
        matches!(cleanup, Err(Error::Protocol | Error::Deadline)),
        "forced cleanup may report the worker fault only after reaping: {cleanup:?}"
    );
}

#[test]
fn confined_program_worker_is_reaped_after_privacy_revocation() {
    let _serial = serialize_process_test();
    if platform::availability().is_err() {
        return;
    }
    let revoked = Arc::new(AtomicBool::new(false));
    let forced = Arc::new(AtomicBool::new(false));
    let mut program = ProgramSupervisor::spawn_with_authority(
        &worker(),
        developer_echo_program().unwrap(),
        11,
        initial_size(),
        forced.clone(),
        Arc::new(Fence {
            cancelled: revoked.clone(),
        }),
    )
    .unwrap();
    assert_eq!(program.input(b"before").unwrap(), vec![b"before".to_vec()]);
    revoked.store(true, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if program.input(b"after").is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(program.input(b"late").is_err());
    assert!(
        !forced.load(Ordering::Acquire),
        "fence-only revocation must not depend on forced cancellation"
    );
    let started = Instant::now();
    let cleanup = std::thread::spawn(move || program.shutdown())
        .join()
        .expect("revoked worker cleanup must be bounded");
    assert!(
        cleanup.is_err(),
        "a fence-only revocation must be reported, not turned into success"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "revoked worker must be reaped within the bounded cleanup interval"
    );
}

#[test]
fn confined_program_worker_hostile_input_is_force_bounded() {
    let _serial = serialize_process_test();
    if platform::program_availability().is_err() {
        return;
    }
    let source = ProgramSourceSnapshot {
        manifest: ProgramManifest {
            version: 1,
            api_version: 1,
            kind: ProgramKind::TerminalProgram,
            name: "hostile-input".into(),
            entry: "main.js".into(),
            modules: vec!["main.js".into()],
            capabilities: BTreeSet::from([
                ProgramCapability::TerminalInput,
                ProgramCapability::TerminalOutput,
                ProgramCapability::TerminalResize,
            ]),
            assets: Vec::new(),
        },
        modules: BTreeMap::from([(
            "main.js".into(),
            r#"export function main({terminal}) {
                terminal.events(event => { if (event.type === "input") while (true) {} });
            }"#
            .into(),
        )]),
    };
    let mut program = ProgramSupervisor::spawn(
        &worker(),
        ApprovedProgram::new(source).unwrap(),
        10,
        initial_size(),
    )
    .unwrap();
    let started = Instant::now();
    assert!(program.input(b"loop").is_err());
    assert!(started.elapsed() < Duration::from_secs(2));
    let cleanup = program.shutdown();
    assert!(
        matches!(
            cleanup,
            Err(Error::Protocol | Error::Deadline | Error::WorkerFailed(_))
        ),
        "hostile turn failure is retained after bounded reaping: {cleanup:?}"
    );
}

#[test]
fn javascript_program_worker_refuses_extra_arguments_exactly() {
    let _serial = serialize_process_test();
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
