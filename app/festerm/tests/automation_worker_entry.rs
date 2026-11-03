use festerm_automation_js::{
    protocol::{read_frame, Limits, Outcome, WorkerFailure, WorkerMessage},
    snapshot::{ApprovedBundle, Manifest, Parameters, SourceSnapshot},
    Event, Supervisor, WorkerProgram,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
    path::Path,
    process::{Command, Output, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

fn run_worker(arguments: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_festerm"))
        .args(arguments)
        .env_clear()
        .env("MALLOC_ARENA_MAX", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the application worker entry");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if child.try_wait().expect("observe worker exit").is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            thread::spawn(move || {
                let _ = child.wait();
            });
            panic!("application worker entry did not exit");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let (send, receive) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let _ = send.send(child.wait_with_output());
    });
    let output = receive
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .expect("worker output did not close before the deadline")
        .expect("collect worker output");
    reader.join().expect("worker output reader failed");
    output
}

#[test]
fn worker_argument_with_extra_arguments_refuses_before_gui_startup() {
    let output = run_worker(&["--automation-worker", "--extra"]);
    assert_eq!(output.status.code(), Some(64));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn application_worker_reports_qualification_before_receiving_source() {
    let output = run_worker(&["--automation-worker"]);
    assert_eq!(output.status.code(), Some(70));
    assert!(output.stderr.is_empty());

    let mut frames = Cursor::new(&output.stdout);
    let first: WorkerMessage = read_frame(&mut frames).expect("first worker frame");
    match first {
        WorkerMessage::Failed {
            code: WorkerFailure::Unavailable,
        } => {}
        WorkerMessage::Ready { profile } if cfg!(target_os = "linux") => {
            assert_eq!(profile, festerm_automation_js::platform::LINUX_PROFILE);
            assert!(matches!(
                read_frame::<_, WorkerMessage>(&mut frames).expect("loading deadline frame"),
                WorkerMessage::Failed {
                    code: WorkerFailure::Deadline
                }
            ));
            let program = Path::new(env!("CARGO_BIN_EXE_festerm"))
                .canonicalize()
                .expect("canonical application binary");
            let program =
                WorkerProgram::from_install_owned_absolute(&program).expect("worker program");
            let bundle = ApprovedBundle::new(SourceSnapshot {
                manifest: Manifest {
                    version: 1,
                    api_version: 1,
                    name: "entry-test".into(),
                    entry: "main.js".into(),
                    modules: vec!["main.js".into()],
                    parameters: BTreeMap::new(),
                    capabilities: BTreeSet::new(),
                },
                modules: BTreeMap::from([(
                    "main.js".into(),
                    "export function main(){ return 42; }".into(),
                )]),
            })
            .expect("worker fixture");
            let worker = Supervisor::spawn(&program, &bundle, Parameters::new(), Limits::default())
                .expect("start qualified application worker");
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                assert!(
                    Instant::now() < deadline,
                    "application worker did not finish"
                );
                match worker.try_event().expect("application worker event") {
                    Some(Event::Finished {
                        outcome: Outcome::Completed { value },
                    }) => {
                        assert_eq!(value, 42);
                        break;
                    }
                    Some(event) => panic!("unexpected application worker event: {event:?}"),
                    None => thread::sleep(Duration::from_millis(1)),
                }
            }
            worker.shutdown().expect("reap application worker");
        }
        message => panic!("unexpected initial worker frame: {message:?}"),
    }
    assert_eq!(frames.position() as usize, output.stdout.len());
}
