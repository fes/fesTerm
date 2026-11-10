use super::*;
use std::cell::RefCell;

struct FakeRunner {
    definition: PathBuf,
    state: RefCell<FakeState>,
}

#[derive(Default)]
struct FakeState {
    registered: bool,
    running: bool,
    refuse_start: bool,
    refuse_registration: bool,
    observation_error: bool,
    native_xml: Option<String>,
    native_security: Option<String>,
    calls: Vec<NativeRequest>,
}

impl CommandRunner for FakeRunner {
    fn execute(&self, request: &NativeRequest) -> io::Result<NativeOutput> {
        let mut state = self.state.borrow_mut();
        state.calls.push(request.clone());
        let mut output = NativeOutput {
            success: true,
            code: 0,
            stdout: Vec::new(),
        };
        match request {
            NativeRequest::Process { args, .. } => {
                let args: Vec<_> = args.iter().map(|arg| arg.to_str().unwrap()).collect();
                if args.contains(&"daemon-reload") {
                    state.registered = self.definition.exists() && !state.refuse_registration;
                }
                if (args.contains(&"start") || args.contains(&"kickstart")) && !state.refuse_start {
                    state.running = true;
                }
                if args.contains(&"stop") {
                    state.running = false;
                }
                if args.contains(&"bootstrap") {
                    state.registered = true;
                }
                if args.contains(&"bootout") {
                    state.registered = false;
                    state.running = false;
                }
                if args.contains(&"show") {
                    let (load, fragment, active, sub, pid) = if !state.registered {
                        ("not-found", "", "inactive", "dead", "0")
                    } else if state.running {
                        (
                            "loaded",
                            self.definition.to_str().unwrap(),
                            "active",
                            "running",
                            "123",
                        )
                    } else {
                        (
                            "loaded",
                            self.definition.to_str().unwrap(),
                            "inactive",
                            "dead",
                            "0",
                        )
                    };
                    output.stdout = format!(
                        "LoadState={load}\nFragmentPath={fragment}\nDropInPaths=\nActiveState={active}\nSubState={sub}\nExecMainPID={pid}\nExecMainStatus=0\n"
                    ).into_bytes();
                    if state.observation_error {
                        output.success = false;
                        output.code = 1;
                    }
                }
                if args.first() == Some(&"print") {
                    if !state.registered {
                        output.success = false;
                        output.code = 113;
                    } else {
                        let record_path = self
                            .definition
                            .parent()
                            .unwrap()
                            .parent()
                            .unwrap()
                            .join("state")
                            .join(RECORD);
                        let record: Registration =
                            serde_json::from_slice(&std::fs::read(record_path).unwrap()).unwrap();
                        output.stdout = format!(
                            "path = {}\nprogram = {}\narguments = {{\n{}\nrun\n--state-dir\n{}\n}}\nstate = {}\n{}",
                            self.definition.display(), record.executable.display(), record.executable.display(),
                            record.state.display(), if state.running { "running" } else { "waiting" },
                            if state.running { "pid = 123\n" } else { "" }).into_bytes();
                    }
                }
            }
            NativeRequest::Task { action, .. } => {
                match action {
                    TaskAction::Create(xml) => {
                        if state.native_xml.is_some() {
                            return Err(refused("fixture.task_exists"));
                        }
                        state.native_xml = Some(format!("normalized:{xml}"));
                    }
                    TaskAction::Start => {
                        if !state.refuse_start {
                            state.running = true;
                        }
                    }
                    TaskAction::Stop => state.running = false,
                    TaskAction::Remove => state.native_xml = None,
                    TaskAction::Query => {}
                }
                output.stdout = serde_json::to_vec(&TaskSnapshot {
                    xml: state.native_xml.clone(),
                    security: state.native_xml.as_ref().map(|_| {
                        state
                            .native_security
                            .clone()
                            .unwrap_or_else(|| "fixture:owner-and-system-only".into())
                    }),
                    state: if state.running { 4 } else { 3 },
                    last_exit: 0,
                })
                .unwrap();
            }
        }
        Ok(output)
    }
}

fn settings() -> Settings {
    Settings {
        friendly_name: "Native fixture".into(),
        allow_creation: false,
        startup: Startup::Manual,
    }
}

fn binding() -> Binding {
    Binding {
        host_id: uuid::Uuid::new_v4().to_string(),
        account: "fixture-account".into(),
        qualified_tunnel: "fixture-qualified".into(),
        cloud_incarnation: "fixture-incarnation".into(),
        host_key_fingerprint: "fixture-key".into(),
    }
}

fn services<'a>(root: &Path, store: &'a Store, platform: Platform) -> Services<'a, FakeRunner> {
    let context = Context {
        platform,
        user: if platform == Platform::Windows {
            "S-1-5-21-1-2-3-1001".into()
        } else {
            "1001".into()
        },
        registration: root.join("registration"),
    };
    let snapshot = store.snapshot().unwrap();
    let extension = if platform == Platform::Macos {
        "plist"
    } else {
        "service"
    };
    let definition = context.registration.join(format!(
        "{}.{}",
        definitions::resource(&snapshot, snapshot.epoch),
        extension
    ));
    Services {
        store,
        state: root.join("state"),
        context,
        runner: FakeRunner {
            definition,
            state: RefCell::new(FakeState::default()),
        },
    }
}

fn provision(store: &Store) -> Snapshot {
    let ticket = store
        .request_enable(&store.snapshot().unwrap().settings)
        .unwrap();
    store.record_provisioned(&ticket, binding()).unwrap();
    store.snapshot().unwrap()
}

#[test]
fn native_service_install_start_stop_cleanup_requires_observed_exact_owner_and_epochs() {
    for (platform, startup) in [Platform::Linux, Platform::Macos, Platform::Windows]
        .into_iter()
        .flat_map(|platform| {
            [Startup::Manual, Startup::UserSignIn].map(|startup| (platform, startup))
        })
    {
        #[cfg(windows)]
        if platform != Platform::Windows {
            continue;
        }
        let root = storage::private_test_directory();
        let store = Store::create(
            &root.path().join("state"),
            Origin::Headless,
            Settings {
                startup,
                ..settings()
            },
        )
        .unwrap();
        let snapshot = provision(&store);
        let stage = trusted::fixture(root.path());
        let services = services(root.path(), &store, platform);
        assert_eq!(
            services.status().err().unwrap().to_string(),
            "host.service_not_installed_no_native_observation"
        );
        let installed = services.install(&stage, snapshot.epoch).unwrap();
        assert_ne!(installed.process, ProcessState::Running);
        let record = services.registration().unwrap().1.unwrap();
        match platform {
            Platform::Linux => assert_eq!(
                services
                    .runner
                    .state
                    .borrow()
                    .calls
                    .iter()
                    .any(|request| matches!(request,
                    NativeRequest::Process { args, .. } if args.iter().any(|arg| arg == "enable"))),
                startup == Startup::UserSignIn
            ),
            Platform::Macos => {
                assert!(record
                    .definition
                    .contains(if startup == Startup::UserSignIn {
                        "<key>RunAtLoad</key><true/>"
                    } else {
                        "<key>RunAtLoad</key><false/>"
                    }))
            }
            Platform::Windows => {
                assert_eq!(
                    record.definition.contains("<LogonTrigger>"),
                    startup == Startup::UserSignIn
                );
                assert!(record
                    .definition
                    .contains("<RunLevel>LeastPrivilege</RunLevel>"));
                assert!(record
                    .definition
                    .contains("<LogonType>InteractiveToken</LogonType>"));
            }
        }
        assert!(!services
            .runner
            .state
            .borrow()
            .calls
            .iter()
            .any(|request| match request {
                NativeRequest::Process { args, .. } => args
                    .iter()
                    .any(|arg| arg == "start" || arg == "kickstart" || arg == "bootstrap"),
                NativeRequest::Task { action, .. } => *action == TaskAction::Start,
            }));
        let mut unqualified = record;
        unqualified.settings.startup = Startup::BeforeLogin;
        assert!(definitions::render(&unqualified).is_err());
        let started = services
            .start_with(&stage, snapshot.epoch, || Ok(()))
            .unwrap();
        assert_eq!(started.process, ProcessState::Running);
        assert_eq!(started.installation, snapshot.installation);
        assert_eq!(store.snapshot().unwrap().phase, Phase::Provisioned);
        store.revoke().unwrap();
        let revoked = store.snapshot().unwrap();
        assert!(services.stop(snapshot.epoch).is_err());
        assert!(services.cleanup(revoked.epoch).is_err());
        assert!(matches!(
            services.stop(revoked.epoch).unwrap().process,
            ProcessState::Stopped | ProcessState::Absent
        ));
        store
            .confirm_stopped(revoked.epoch, revoked.binding.as_ref().unwrap())
            .unwrap();
        store.request_cleanup().unwrap();
        let epoch = store.snapshot().unwrap().epoch;
        assert_eq!(
            services.cleanup(epoch).unwrap().process,
            ProcessState::Absent
        );
        assert_eq!(
            services.cleanup(epoch).unwrap().process,
            ProcessState::Absent
        );
        assert_eq!(services.status().unwrap().process, ProcessState::Absent);
        assert_eq!(store.snapshot().unwrap().phase, Phase::CleanupPending);
        assert!(root.path().join("state").join(RECORD).exists());
        assert!(stage.path().exists());
        assert!(!services
            .runner
            .state
            .borrow()
            .calls
            .iter()
            .any(|request| match request {
                NativeRequest::Process { tool, args } =>
                    tool.to_string_lossy().contains("sh")
                        || args
                            .iter()
                            .any(|arg| arg == "--system" || arg == "enable-linger"),
                _ => false,
            }));
    }
}

#[test]
fn native_service_locked_vault_failed_start_and_foreign_definitions_fail_closed() {
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        #[cfg(windows)]
        if platform != Platform::Windows {
            continue;
        }
        let root = storage::private_test_directory();
        let store =
            Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
        let snapshot = provision(&store);
        let stage = trusted::fixture(root.path());
        let services = services(root.path(), &store, platform);
        if platform == Platform::Linux {
            services.runner.state.borrow_mut().refuse_registration = true;
            assert_eq!(
                services
                    .install(&stage, snapshot.epoch)
                    .err()
                    .unwrap()
                    .to_string(),
                "host.service_registration_not_observed"
            );
            services.runner.state.borrow_mut().refuse_registration = false;
        }
        services.install(&stage, snapshot.epoch).unwrap();
        if platform == Platform::Windows {
            services.runner.state.borrow_mut().native_security = Some("foreign".into());
            assert!(services.status().is_err());
            services.runner.state.borrow_mut().native_security = None;
        }
        if platform == Platform::Linux {
            services.runner.state.borrow_mut().observation_error = true;
            assert!(services.status().is_err());
            services.runner.state.borrow_mut().observation_error = false;
        }
        assert_eq!(
            services
                .start_with(&stage, snapshot.epoch, || Err(refused(
                    "owner.vault_locked_or_unavailable_unlock_native_store_and_retry"
                )))
                .err()
                .unwrap()
                .to_string(),
            "owner.vault_locked_or_unavailable_unlock_native_store_and_retry"
        );
        services.runner.state.borrow_mut().refuse_start = true;
        assert!(services
            .start_with(&stage, snapshot.epoch, || Ok(()))
            .is_err());
        assert_ne!(services.status().unwrap().process, ProcessState::Running);
        if platform == Platform::Windows {
            services.runner.state.borrow_mut().native_xml = Some("foreign".into());
        } else {
            let path = &services.runner.definition;
            std::fs::write(path, b"foreign").unwrap();
        }
        assert!(services.status().is_err());
        store.revoke().unwrap();
        assert!(services.stop(store.snapshot().unwrap().epoch).is_err());
        assert_eq!(store.snapshot().unwrap().phase, Phase::StopRequested);
    }
}

struct RevokingRunner<'a> {
    native: FakeRunner,
    store: &'a Store,
    creation: bool,
}
impl CommandRunner for RevokingRunner<'_> {
    fn execute(&self, request: &NativeRequest) -> io::Result<NativeOutput> {
        let output = self.native.execute(request)?;
        let revoke = if self.creation {
            matches!(
                request,
                NativeRequest::Task {
                    action: TaskAction::Create(_),
                    ..
                }
            )
        } else {
            matches!(request, NativeRequest::Process { args, .. } if args.iter().any(|arg| arg == "start" || arg == "kickstart"))
                || matches!(
                    request,
                    NativeRequest::Task {
                        action: TaskAction::Start,
                        ..
                    }
                )
        };
        if revoke {
            self.store.revoke()?;
        }
        Ok(output)
    }
}

#[cfg(unix)]
#[test]
fn native_service_late_start_after_revocation_retains_exact_cleanup_receipt_without_success() {
    let root = storage::private_test_directory();
    let store = Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
    let snapshot = provision(&store);
    let stage = trusted::fixture(root.path());
    let base = services(root.path(), &store, Platform::Linux);
    let services = Services {
        store: &store,
        state: base.state,
        context: base.context,
        runner: RevokingRunner {
            native: base.runner,
            store: &store,
            creation: false,
        },
    };
    services.install(&stage, snapshot.epoch).unwrap();
    assert!(services
        .start_with(&stage, snapshot.epoch, || Ok(()))
        .is_err());
    let revoked = store.snapshot().unwrap();
    assert_eq!(revoked.phase, Phase::StopRequested);
    assert_eq!(services.status().unwrap().process, ProcessState::Running);
    assert_eq!(
        services.stop(revoked.epoch).unwrap().process,
        ProcessState::Stopped
    );
    let directory = storage::private_directory(&root.path().join("state"), false).unwrap();
    let record: Registration = serde_json::from_slice(
        &storage::read_private(&directory, Path::new(RECORD), 128 * 1024).unwrap(),
    )
    .unwrap();
    assert_eq!(record.installation, snapshot.installation);
    assert_eq!(record.binding, snapshot.binding.unwrap());
    assert!(!record.removed);
}

#[test]
fn native_service_late_task_creation_retains_normalized_identity_for_exact_cleanup() {
    let root = storage::private_test_directory();
    let store = Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
    let snapshot = provision(&store);
    let stage = trusted::fixture(root.path());
    let base = services(root.path(), &store, Platform::Windows);
    let services = Services {
        store: &store,
        state: base.state,
        context: base.context,
        runner: RevokingRunner {
            native: base.runner,
            store: &store,
            creation: true,
        },
    };
    assert!(services.install(&stage, snapshot.epoch).is_err());
    let revoked = store.snapshot().unwrap();
    assert_eq!(revoked.phase, Phase::StopRequested);
    let record = services.registration().unwrap().1.unwrap();
    assert!(record.native_snapshot.is_some());
    assert!(record.native_security.is_some());
    assert_eq!(record.epoch, snapshot.epoch);
    assert_eq!(
        services.stop(revoked.epoch).unwrap().process,
        ProcessState::Stopped
    );
    store
        .confirm_stopped(revoked.epoch, revoked.binding.as_ref().unwrap())
        .unwrap();
    store.request_cleanup().unwrap();
    assert_eq!(
        services
            .cleanup(store.snapshot().unwrap().epoch)
            .unwrap()
            .process,
        ProcessState::Absent
    );
}

struct TimedOutRunner {
    native: FakeRunner,
    outstanding: RefCell<Option<std::sync::Arc<Lease>>>,
}

impl CommandRunner for TimedOutRunner {
    fn execute(&self, request: &NativeRequest) -> io::Result<NativeOutput> {
        let output = self.native.execute(request)?;
        if matches!(
            request,
            NativeRequest::Task {
                action: TaskAction::Create(_),
                ..
            }
        ) {
            *self.outstanding.borrow_mut() = native::retain_operation();
            return Err(refused("host.service_native_timeout"));
        }
        Ok(output)
    }
}

#[test]
fn native_service_outstanding_effect_retains_cross_instance_exclusion_after_timeout() {
    let root = storage::private_test_directory();
    let store = Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
    let snapshot = provision(&store);
    let stage = trusted::fixture(root.path());
    let base = services(root.path(), &store, Platform::Windows);
    let active = Services {
        store: &store,
        state: base.state,
        context: base.context,
        runner: TimedOutRunner {
            native: base.runner,
            outstanding: RefCell::new(None),
        },
    };
    assert!(active.install(&stage, snapshot.epoch).is_err());
    assert!(native::retain_operation().is_none());
    let reopened = Store::open(&root.path().join("state"), Origin::Headless).unwrap();
    reopened.revoke().unwrap();
    let other = services(root.path(), &reopened, Platform::Windows);
    assert_eq!(
        other.status().err().unwrap().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(active.runner.outstanding.borrow_mut().take());
    assert!(active.status().is_err());
    assert!(active
        .registration()
        .unwrap()
        .1
        .unwrap()
        .native_snapshot
        .is_none());
    assert_eq!(reopened.snapshot().unwrap().phase, Phase::StopRequested);
}

#[test]
fn native_service_status_rejects_manager_overrides_ambiguous_properties_and_cross_origin() {
    let path = std::env::current_dir().unwrap().join("owned.service");
    let stopped = format!(    "LoadState=loaded\nFragmentPath={}\nDropInPaths=\nActiveState=inactive\nSubState=dead\nExecMainPID=0\nExecMainStatus=0\n", path.display());
    assert_eq!(
        definitions::linux_state(stopped.as_bytes(), &path).unwrap(),
        ProcessState::Stopped
    );
    assert_eq!(
        definitions::linux_state(
            stopped
                .replace("ExecMainStatus=0", "ExecMainStatus=78")
                .as_bytes(),
            &path
        )
        .unwrap(),
        ProcessState::Failed
    );
    assert!(
        definitions::linux_state(format!("{stopped}LoadState=not-found\n").as_bytes(), &path)
            .is_err()
    );
    assert!(definitions::linux_state(
        stopped
            .replace("DropInPaths=\n", "DropInPaths=/foreign.conf\n")
            .as_bytes(),
        &path
    )
    .is_err());
    let root = storage::private_test_directory();
    let store = Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
    let snapshot = provision(&store);
    let services = services(root.path(), &store, Platform::Windows);
    let stage = trusted::fixture(root.path());
    services.install(&stage, snapshot.epoch).unwrap();
    let directory = storage::private_directory(&root.path().join("state"), false).unwrap();
    let mut record = services.registration().unwrap().1.unwrap();
    record.origin = Origin::Desktop;
    storage::replace_private(
        &directory,
        Path::new(RECORD),
        &serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    assert!(services.status().is_err());
    std::fs::remove_file(root.path().join("state").join(LOCK)).unwrap();
    assert_eq!(
        services.status().err().unwrap().to_string(),
        "host.service_lock_missing_reconciliation_required"
    );
    assert!(!root.path().join("state").join(LOCK).exists());
}
