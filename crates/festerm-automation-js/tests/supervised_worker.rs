use festerm_automation_js::{
    event_wake_channel, platform,
    protocol::{HostRequest, HostResult, Limits, Outcome},
    snapshot::{
        ApprovedBundle, Manifest, ParameterType, ParameterValue, Parameters, SourceSnapshot,
    },
    DisclosureGuard, DisclosurePermit, Error, Event, Supervisor, WorkerProgram,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{mpsc, Arc, Mutex, TryLockError},
    time::{Duration, Instant},
};

struct TestDisclosure {
    state: Arc<Mutex<bool>>,
    attempted: Option<mpsc::SyncSender<()>>,
}

impl DisclosureGuard for TestDisclosure {
    fn try_enter(&self) -> Result<Option<Box<dyn DisclosurePermit + '_>>, Error> {
        match self.state.try_lock() {
            Ok(guard) if *guard => Ok(Some(Box::new(guard))),
            Ok(_) | Err(TryLockError::Poisoned(_)) => Err(Error::DisclosureRevoked),
            Err(TryLockError::WouldBlock) => {
                if let Some(attempted) = &self.attempted {
                    let _ = attempted.try_send(());
                }
                Ok(None)
            }
        }
    }
}

fn disclosure() -> Arc<dyn DisclosureGuard> {
    Arc::new(TestDisclosure {
        state: Arc::new(Mutex::new(true)),
        attempted: None,
    })
}

fn program() -> WorkerProgram {
    let path = Path::new(env!("CARGO_BIN_EXE_festerm-automation-js-worker"))
        .canonicalize()
        .unwrap();
    WorkerProgram::from_install_owned_absolute(&path).unwrap()
}

fn bundle(source: &str) -> ApprovedBundle {
    ApprovedBundle::new(SourceSnapshot {
        manifest: Manifest {
            version: 1,
            api_version: 1,
            name: "test".into(),
            entry: "main.js".into(),
            modules: vec!["main.js".into()],
            parameters: BTreeMap::new(),
            capabilities: BTreeSet::new(),
        },
        modules: BTreeMap::from([("main.js".into(), source.into())]),
    })
    .unwrap()
}

fn terminal(worker: &Supervisor, broker: bool) -> Event {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        assert!(
            Instant::now() < deadline,
            "owned worker event deadline exceeded"
        );
        if let Some(event) = worker.try_event().unwrap() {
            match event {
                Event::HostCall { id, request } => {
                    assert!(broker, "no host call should have been dispatched");
                    assert_eq!(request, HostRequest::Observe {});
                    worker
                        .host_result(
                            id,
                            HostResult::Observation { targets: vec![] },
                            disclosure(),
                        )
                        .unwrap();
                }
                event => return event,
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn worker_events_wake_a_consumer_without_polling() {
    if platform::availability().is_err() {
        return;
    }
    let (wake, notifications) = event_wake_channel();
    let worker = Supervisor::spawn_with_wake(
        &program(),
        &bundle("export async function main({fesTerm}) { await fesTerm.observe(); return 7; }"),
        Parameters::new(),
        Limits::default(),
        wake.clone(),
    )
    .unwrap();
    // The bridge may consume the worker's first signal before registration.
    notifications.recv_timeout(Duration::from_secs(8)).unwrap();
    wake.notify().unwrap();
    notifications.recv_timeout(Duration::from_secs(8)).unwrap();
    let Some(Event::HostCall { id, request }) = worker.try_event().unwrap() else {
        panic!("host call must be queued before its wake");
    };
    assert_eq!(request, HostRequest::Observe {});
    worker
        .host_result(
            id,
            HostResult::Observation { targets: vec![] },
            disclosure(),
        )
        .unwrap();
    notifications.recv_timeout(Duration::from_secs(8)).unwrap();
    let event = worker.try_event().unwrap();
    assert!(
        matches!(
            &event,
            Some(Event::Finished {
                outcome: Outcome::Completed { value }
            }) if value == 7
        ),
        "unexpected worker event: {event:?}"
    );
    worker.shutdown().unwrap();
}

#[test]
fn worker_survives_spawn_caller_thread_exit() {
    if platform::availability().is_err() {
        return;
    }
    let worker = std::thread::spawn(|| {
        Supervisor::spawn(
            &program(),
            &bundle("export async function main() { return 7; }"),
            Parameters::new(),
            Limits::default(),
        )
    })
    .join()
    .unwrap()
    .unwrap();
    assert!(matches!(
        terminal(&worker, false),
        Event::Finished {
            outcome: Outcome::Completed { value }
        } if value == 7
    ));
    worker.shutdown().unwrap();
}

#[test]
fn undrained_wake_does_not_block_worker_deadline_or_cleanup() {
    if platform::availability().is_err() {
        return;
    }
    let (wake, notifications) = event_wake_channel();
    wake.notify().unwrap();
    let worker = Supervisor::spawn_with_wake(
        &program(),
        &bundle("export async function main({fesTerm}) { await fesTerm.observe(); }"),
        Parameters::new(),
        Limits {
            run_ms: 500,
            ..Limits::default()
        },
        wake,
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(750));
    notifications.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(matches!(
        terminal(&worker, false),
        Event::Failed(Error::Deadline)
    ));
    worker.shutdown().unwrap();
}

#[test]
fn losing_the_wake_receiver_closes_the_worker() {
    if platform::availability().is_err() {
        return;
    }
    let (wake, notifications) = event_wake_channel();
    drop(notifications);
    let worker = Supervisor::spawn_with_wake(
        &program(),
        &bundle("export async function main({fesTerm}) { await fesTerm.observe(); }"),
        Parameters::new(),
        Limits::default(),
        wake,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        assert!(Instant::now() < deadline);
        match worker.try_event().unwrap() {
            Some(Event::Failed(Error::Unavailable("worker event wake receiver closed"))) => break,
            Some(Event::HostCall { .. }) | None => std::thread::sleep(Duration::from_millis(1)),
            Some(event) => panic!("worker ended unexpectedly: {event:?}"),
        }
    }
    worker.shutdown().unwrap();
}

#[test]
fn target_parameter_reaches_worker_as_exact_bound_handle() {
    if platform::availability().is_err() {
        return;
    }
    let mut snapshot = bundle(
        "export async function main({fesTerm,params}) { \
         const result = await fesTerm.readScreen({target: params.target, maxBytes: 12}); \
         return result.text; }",
    )
    .snapshot()
    .clone();
    snapshot
        .manifest
        .parameters
        .insert("target".into(), ParameterType::Target {});
    let bundle = ApprovedBundle::new(snapshot).unwrap();
    let parameters = bundle
        .bind_parameters(
            BTreeMap::from([("target".into(), ParameterValue::Target("selected".into()))]),
            BTreeMap::from([("selected".into(), 42)]),
        )
        .unwrap();
    let worker =
        Supervisor::spawn_bound(&program(), &bundle, &parameters, Limits::default()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        assert!(Instant::now() < deadline);
        match worker.try_event().unwrap() {
            Some(Event::HostCall { id, request }) => {
                assert_eq!(
                    request,
                    HostRequest::ReadScreen {
                        target: festerm_automation_js::protocol::Target {
                            id: "selected".into(),
                            generation: 42,
                        },
                        max_bytes: 12,
                    }
                );
                worker
                    .host_result(
                        id,
                        HostResult::Text {
                            text: "ready".into(),
                            revision: 1,
                            truncated: false,
                        },
                        disclosure(),
                    )
                    .unwrap();
                break;
            }
            Some(event) => panic!("worker ended before its bound read: {event:?}"),
            None => std::thread::sleep(Duration::from_millis(1)),
        }
    }
    assert!(matches!(
        terminal(&worker, false),
        Event::Finished {
            outcome: Outcome::Completed { value }
        } if value == "ready"
    ));
    worker.shutdown().unwrap();
}

#[test]
fn unsupported_platform_never_launches_or_executes_script() {
    if platform::availability().is_ok() {
        return;
    }
    let result = Supervisor::spawn(
        &program(),
        &bundle("export function main(){while(true){}}"),
        Parameters::new(),
        Limits::default(),
    );
    assert!(matches!(result, Err(Error::Unavailable(_))));
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_festerm-automation-js-worker"))
        .arg("--automation-worker")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(status.status.code(), Some(70));
    assert!(matches!(
        festerm_automation_js::protocol::read_frame::<
            _,
            festerm_automation_js::protocol::WorkerMessage,
        >(&mut &status.stdout[..])
        .unwrap(),
        festerm_automation_js::protocol::WorkerMessage::Failed {
            code: festerm_automation_js::protocol::WorkerFailure::Unavailable,
        }
    ));
    assert!(status.stderr.is_empty());
}

#[test]
fn native_policy_and_hard_memory_ceiling_precede_first_script_instruction() {
    if platform::availability().is_err() {
        return;
    }
    // Ready is emitted only after actual native read/write/network/raw-socket/
    // clone/exec/ptrace/IPC/unexpected-descriptor and hard-AS denial probes.
    let worker = Supervisor::spawn(&program(),
        &bundle("export async function main({fesTerm}) { return (await fesTerm.observe()).targets.length; }"),
        Parameters::new(), Limits::default()).unwrap();
    match terminal(&worker, true) {
        Event::Finished {
            outcome: Outcome::Completed { value },
        } => assert_eq!(value, 0),
        other => panic!("worker qualification/execution failed: {other:?}"),
    }
    worker.shutdown().unwrap();
}

#[test]
fn runaway_jobs_unsettled_calls_and_private_errors_are_contained() {
    if platform::availability().is_err() {
        return;
    }
    for (source, unsettled) in [
        ("export function main(){while(true){}}", false),
        ("export function main(){function f(){f()}f()}", false),
        (
            "export function main(){let a=[];while(true)a.push(new Uint8Array(1024*1024))}",
            false,
        ),
        (
            "export function main(){return /^(a+)+$/.test('a'.repeat(100)+'!')}",
            false,
        ),
        (
            "export async function main(){while(true)await Promise.resolve()}",
            false,
        ),
        (
            "export async function main({fesTerm}){fesTerm.observe();return 1}",
            true,
        ),
        (
            "export function main(){throw new Error('PRIVATE-SCRIPT-DATA')}",
            false,
        ),
    ] {
        let limits = Limits {
            cpu_ms: 100,
            run_ms: 1_500,
            ..Limits::default()
        };
        let worker =
            Supervisor::spawn(&program(), &bundle(source), Parameters::new(), limits).unwrap();
        let event = terminal(&worker, false);
        assert!(!format!("{event:?}").contains("PRIVATE-SCRIPT-DATA"));
        match event {
            Event::Finished {
                outcome: Outcome::UnsettledWork {},
            } if unsettled => {}
            Event::Finished {
                outcome: Outcome::ResourceLimit {} | Outcome::ScriptFailed {},
            } if !unsettled => {}
            Event::Failed(Error::Deadline) if !unsettled => {}
            event => panic!("unexpected hostile script outcome for {source}: {event:?}"),
        }
        worker.shutdown().unwrap();
    }
}

#[test]
fn cancellation_closes_admission_and_reaps_within_bounds() {
    if platform::availability().is_err() {
        return;
    }
    let worker = Supervisor::spawn(
        &program(),
        &bundle("export async function main({fesTerm}) { await fesTerm.observe(); }"),
        Parameters::new(),
        Limits::default(),
    )
    .unwrap();
    worker.cancel();
    assert!(worker
        .with_admission(|_| panic!("cancelled admission ran"))
        .is_err());
    assert!(worker
        .host_result(1, HostResult::Observation { targets: vec![] }, disclosure())
        .is_err());
    let started = Instant::now();
    worker.shutdown().unwrap();
    assert!(started.elapsed() < Duration::from_millis(2_500));
}

#[test]
fn independently_revoked_result_never_reaches_the_worker() {
    if platform::availability().is_err() {
        return;
    }
    let worker = Supervisor::spawn(
        &program(),
        &bundle(
            "export async function main({fesTerm}) { \
             return (await fesTerm.observe()).targets[0].target.id; }",
        ),
        Parameters::new(),
        Limits::default(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let id = loop {
        assert!(Instant::now() < deadline);
        match worker.try_event().unwrap() {
            Some(Event::HostCall { id, request }) => {
                assert_eq!(request, HostRequest::Observe {});
                break id;
            }
            Some(event) => panic!("worker ended before the host call: {event:?}"),
            None => std::thread::sleep(Duration::from_millis(1)),
        }
    };
    let state = Arc::new(Mutex::new(true));
    let (locked, observed_lock) = mpsc::sync_channel(0);
    let (revoke, await_revocation) = mpsc::sync_channel(0);
    let target_state = state.clone();
    let transition = std::thread::spawn(move || {
        let mut held = target_state.lock().unwrap();
        locked.send(()).unwrap();
        await_revocation.recv().unwrap();
        *held = false;
    });
    observed_lock.recv_timeout(Duration::from_secs(2)).unwrap();
    let (attempted, observed_attempt) = mpsc::sync_channel(1);
    let guard = Arc::new(TestDisclosure {
        state: state.clone(),
        attempted: Some(attempted),
    });
    worker
        .host_result(
            id,
            HostResult::Observation {
                targets: vec![festerm_automation_js::protocol::TargetInfo {
                    target: festerm_automation_js::protocol::Target {
                        id: "private".into(),
                        generation: 1,
                    },
                    revision: 1,
                }],
            },
            guard,
        )
        .unwrap();
    observed_attempt
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    revoke.send(()).unwrap();
    transition.join().unwrap();
    assert!(matches!(
        terminal(&worker, false),
        Event::Failed(Error::DisclosureRevoked)
    ));
    worker.shutdown().unwrap();
}

#[test]
fn host_admission_acknowledgement_unblocks_concurrent_calls() {
    if platform::availability().is_err() {
        return;
    }
    let worker = Supervisor::spawn(
        &program(),
        &bundle(
            "export async function main({fesTerm}) { \
             fesTerm.observe(); await fesTerm.observe(); return 1; }",
        ),
        Parameters::new(),
        Limits::default(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut ids = Vec::new();
    while ids.len() < 2 {
        assert!(Instant::now() < deadline);
        match worker.try_event().unwrap() {
            Some(Event::HostCall { id, request }) => {
                assert_eq!(request, HostRequest::Observe {});
                ids.push(id);
            }
            Some(event) => panic!("worker ended before both calls: {event:?}"),
            None => std::thread::sleep(Duration::from_millis(1)),
        }
    }
    assert_eq!(ids, [1, 2]);
    worker
        .host_result(2, HostResult::Observation { targets: vec![] }, disclosure())
        .unwrap();
    worker.host_admitted(1).unwrap();
    assert!(matches!(
        terminal(&worker, false),
        Event::Finished {
            outcome: Outcome::UnsettledWork {}
        }
    ));
    worker.shutdown().unwrap();
}

#[test]
fn result_before_separate_acknowledgement_does_not_fail_the_run() {
    if platform::availability().is_err() {
        return;
    }
    let worker = Supervisor::spawn(
        &program(),
        &bundle(
            "export async function main({fesTerm}) { \
             const first = fesTerm.observe(); const second = fesTerm.observe(); \
             await first; await second; return 42; }",
        ),
        Parameters::new(),
        Limits::default(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut ids = Vec::new();
    while ids.len() < 2 {
        assert!(Instant::now() < deadline);
        match worker.try_event().unwrap() {
            Some(Event::HostCall { id, request }) => {
                assert_eq!(request, HostRequest::Observe {});
                ids.push(id);
            }
            Some(event) => panic!("worker ended before both calls: {event:?}"),
            None => std::thread::sleep(Duration::from_millis(1)),
        }
    }
    assert_eq!(ids, [1, 2]);
    worker
        .host_result(1, HostResult::Observation { targets: vec![] }, disclosure())
        .unwrap();
    worker.host_admitted(1).unwrap();
    worker
        .host_result(2, HostResult::Observation { targets: vec![] }, disclosure())
        .unwrap();
    assert!(matches!(
        terminal(&worker, false),
        Event::Finished {
            outcome: Outcome::Completed { value }
        } if value == 42
    ));
    worker.shutdown().unwrap();
}

#[test]
fn watchdog_is_independent_of_application_polling() {
    if platform::availability().is_err() {
        return;
    }
    let worker = Supervisor::spawn(
        &program(),
        &bundle("export async function main({fesTerm}) { await fesTerm.observe(); }"),
        Parameters::new(),
        Limits {
            run_ms: 250,
            ..Limits::default()
        },
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(matches!(
        terminal(&worker, false),
        Event::Failed(Error::Deadline)
    ));
    worker.shutdown().unwrap();
}
