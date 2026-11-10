use super::*;
use std::cell::RefCell;

#[cfg(all(
    feature = "native-service-fixtures",
    any(target_os = "linux", target_os = "macos")
))]
#[test]
#[ignore = "mutates exact-owned native services: disposable guest and explicit lab inputs required"]
fn native_service_live_exact_owned_lifecycle_preserves_independent_child() {
    use std::time::{Duration, Instant};
    assert_eq!(
        std::env::var("FESTERM_ADR84_NATIVE_LAB").as_deref(),
        Ok("1")
    );
    let root = PathBuf::from(std::env::var_os("FESTERM_ADR84_NATIVE_ROOT").unwrap());
    let child = PathBuf::from(std::env::var_os("FESTERM_ADR84_NATIVE_CHILD").unwrap());
    assert!(root.is_absolute() && child.is_absolute());
    assert!(
        !root.exists(),
        "preserve prior evidence; use a fresh private root"
    );
    assert_eq!(
        child.file_name().unwrap(),
        "festerm-host-service-test-child"
    );
    assert!(std::fs::symlink_metadata(&child).unwrap().is_file());
    let bytes = std::fs::read(&child).unwrap();
    assert!(!bytes.is_empty() && bytes.len() <= crate::artifacts::MAX_ENTRY_BYTES as usize);
    let directory = storage::private_directory(&root, true).unwrap();
    let state = root.join("state");
    let store = Store::create(&state, Origin::Headless, settings()).unwrap();
    let snapshot = provision(&store);
    let services = Services::current_user(&store, &state).unwrap();
    struct Cleanup<'a> {
        services: &'a Services<'a>,
        state: &'a Path,
    }
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = (|| -> io::Result<()> {
                let store = self.services.store;
                let snapshot = store.snapshot()?;
                if snapshot.phase == Phase::Provisioned {
                    store.revoke()?;
                }
                let snapshot = store.snapshot()?;
                if snapshot.phase == Phase::StopRequested {
                    self.services.stop(snapshot.epoch)?;
                    store.confirm_stopped(snapshot.epoch, snapshot.binding.as_ref().unwrap())?;
                }
                if store.snapshot()?.phase == Phase::Disabled {
                    store.request_cleanup()?;
                }
                let snapshot = store.snapshot()?;
                if snapshot.phase == Phase::CleanupPending {
                    self.services.cleanup(snapshot.epoch)?;
                }
                Ok(())
            })();
            if let Ok(directory) = storage::private_directory(self.state, false) {
                let _ = storage::write_new_private(&directory, Path::new("fixture-stop"), b"stop");
            }
        }
    }
    let _cleanup = Cleanup {
        services: &services,
        state: &state,
    };
    let stage = trusted::lab_fixture(&root, &bytes);
    services.install(&stage, snapshot.epoch).unwrap();
    assert!(
        !state.join("fixture-host-started").exists(),
        "install must launch no process"
    );
    let observed = services
        .start_with(&stage, snapshot.epoch, || Ok(()))
        .unwrap();
    assert_eq!(observed.process, ProcessState::Running);
    let wait = |check: &dyn Fn() -> bool| {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !check() {
            assert!(
                Instant::now() < deadline,
                "native fixture observation deadline"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    wait(&|| {
        state.join("fixture-host-started").exists()
            && state.join("fixture-daemon-heartbeat").exists()
    });
    store.revoke().unwrap();
    let revoked = store.snapshot().unwrap();
    services.stop(revoked.epoch).unwrap();
    store
        .confirm_stopped(revoked.epoch, revoked.binding.as_ref().unwrap())
        .unwrap();
    store.request_cleanup().unwrap();
    let pending = store.snapshot().unwrap();
    services.cleanup(pending.epoch).unwrap();
    assert_eq!(services.status().unwrap().process, ProcessState::Absent);
    let heartbeat = std::fs::read(state.join("fixture-daemon-heartbeat")).unwrap();
    wait(&|| {
        std::fs::read(state.join("fixture-daemon-heartbeat"))
            .is_ok_and(|current| current != heartbeat)
    });
    store
        .confirm_removed(pending.epoch, pending.binding.as_ref().unwrap())
        .unwrap();
    let state_directory = storage::private_directory(&state, false).unwrap();
    storage::write_new_private(&state_directory, Path::new("fixture-stop"), b"stop").unwrap();
    wait(&|| state.join("fixture-daemon-stopped").exists());
    storage::write_new_private(
        &directory,
        Path::new("native-lifecycle-qualified"),
        b"manager-only; synthetic fixture, not publisher/vault/cloud acceptance",
    )
    .unwrap();
}

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
    failed: bool,
    fail_reload_once: bool,
    pending_start: bool,
    start_delay_queries: usize,
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
                    if state.fail_reload_once {
                        state.fail_reload_once = false;
                        output.success = false;
                    }
                }
                if (args.contains(&"start") || args.contains(&"kickstart")) && !state.refuse_start {
                    if state.start_delay_queries > 0 {
                        state.pending_start = true;
                    } else {
                        state.running = true;
                    }
                }
                if args.contains(&"reset-failed") {
                    state.failed = false;
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
                        if state.failed {
                            ("not-found", "", "failed", "failed", "0")
                        } else {
                            ("not-found", "", "inactive", "dead", "0")
                        }
                    } else if state.failed {
                        (
                            "loaded",
                            self.definition.to_str().unwrap(),
                            "failed",
                            "failed",
                            "0",
                        )
                    } else if state.pending_start {
                        (
                            "loaded",
                            self.definition.to_str().unwrap(),
                            "activating",
                            "start",
                            "0",
                        )
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
                        "Id={}\nLoadState={load}\nFragmentPath={fragment}\nDropInPaths=\nActiveState={active}\nSubState={sub}\nMainPID={pid}\nExecMainPID=4321\nResult={}\nExecMainCode=1\nExecMainStatus={}\n",
                        self.definition.file_name().unwrap().to_str().unwrap(),
                        if state.failed { "exit-code" } else { "success" },
                        if state.failed { 1 } else { 0 }
                    ).into_bytes();
                    if state.pending_start {
                        state.start_delay_queries = state.start_delay_queries.saturating_sub(1);
                        if state.start_delay_queries == 0 {
                            state.pending_start = false;
                            state.running = true;
                        }
                    }
                    if state.observation_error || !state.registered {
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
                    TaskAction::Prepare(xml) => {
                        output.stdout = serde_json::to_vec(&TaskSnapshot {
                            xml: Some(format!("normalized:{xml}")),
                            security: Some(
                                "task-owner-system-full-control-v1:S-1-5-21-1-2-3-1001".into(),
                            ),
                            state: 0,
                            last_exit: 0,
                        })
                        .unwrap();
                        return Ok(output);
                    }
                    TaskAction::Create(xml) => {
                        if state.native_xml.is_some() {
                            return Err(refused("fixture.task_exists"));
                        }
                        state.native_xml = Some(xml.clone());
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
                        state.native_security.clone().unwrap_or_else(|| {
                            "task-owner-system-full-control-v1:S-1-5-21-1-2-3-1001".into()
                        })
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

#[cfg(all(feature = "cloud", feature = "host-runtime"))]
#[test]
#[should_panic(expected = "pure unpublished-registration fixture must not call a native manager")]
fn native_service_unpublished_fixture_forbids_native_manager_effects() {
    let root = storage::private_test_directory();
    let state = root.path().join("state");
    let store = Store::create(&state, Origin::Headless, settings()).unwrap();
    let fixture = super::fixture_services(&store, &state).unwrap();
    let _ = fixture.runner.execute(&NativeRequest::Task {
        name: "fixture-only-never-native".into(),
        action: TaskAction::Query,
    });
}

fn provision(store: &Store) -> Snapshot {
    let ticket = store
        .request_enable(&store.snapshot().unwrap().settings)
        .unwrap();
    store.record_provisioned(&ticket, binding()).unwrap();
    store.snapshot().unwrap()
}

#[test]
fn native_service_unpublished_receipt_proves_only_owned_absence_without_manager_calls() {
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        let root = storage::private_test_directory();
        let state = root.path().join("state");
        let store = Store::create(&state, Origin::Headless, settings()).unwrap();
        let provisioned = provision(&store);
        let services = services(root.path(), &store, platform);
        let registration =
            storage::private_directory(&services.context.registration, true).unwrap();
        let foreign_name = services.runner.definition.file_name().unwrap();
        storage::write_new_private(
            &registration,
            Path::new(foreign_name),
            b"foreign definition",
        )
        .unwrap();
        {
            let mut manager = services.runner.state.borrow_mut();
            manager.registered = true;
            manager.running = true;
        }
        assert!(services.stop(provisioned.epoch).is_err());
        assert!(services.cleanup(provisioned.epoch).is_err());
        store.revoke().unwrap();
        let stopped = store.snapshot().unwrap();
        assert!(services.stop(provisioned.epoch).is_err());
        assert!(services.cleanup(stopped.epoch).is_err());
        let observed = services.stop(stopped.epoch).unwrap();
        assert_eq!(observed.process, ProcessState::Absent);
        assert_eq!(observed.installation, stopped.installation);
        assert_eq!(observed.origin, stopped.origin);
        assert_eq!(observed.epoch, stopped.epoch);
        store
            .confirm_stopped(observed.epoch, stopped.binding.as_ref().unwrap())
            .unwrap();
        assert!(services.stop(observed.epoch).is_err());
        assert!(services.cleanup(observed.epoch).is_err());
        store.request_cleanup().unwrap();
        let pending = store.snapshot().unwrap();
        assert!(services.stop(pending.epoch).is_err());
        assert!(services.cleanup(stopped.epoch).is_err());
        let cleaned = services.cleanup(pending.epoch).unwrap();
        assert_eq!(cleaned.process, ProcessState::Absent);
        assert_eq!(cleaned.installation, pending.installation);
        assert_eq!(cleaned.origin, pending.origin);
        assert_eq!(cleaned.epoch, pending.epoch);
        assert_eq!(
            services.status().err().unwrap().to_string(),
            "host.service_not_installed_no_native_observation"
        );
        assert!(!state.join(RECORD).exists());
        assert_eq!(
            std::fs::read(&services.runner.definition).unwrap(),
            b"foreign definition"
        );
        let manager = services.runner.state.borrow();
        assert!(manager.calls.is_empty());
        assert!(manager.registered && manager.running);
    }
}

#[test]
fn native_service_unpublished_receipt_requires_exclusive_lease_and_current_instance() {
    let root = storage::private_test_directory();
    let state = root.path().join("state");
    let store = Store::create(&state, Origin::Headless, settings()).unwrap();
    provision(&store);
    let mut services = services(root.path(), &store, Platform::Windows);
    store.revoke().unwrap();
    let stopped = store.snapshot().unwrap();
    let lease = services.lease().unwrap();
    assert_eq!(
        services.stop(stopped.epoch).err().unwrap().kind(),
        io::ErrorKind::WouldBlock
    );
    store.revoke().unwrap();
    assert!(services.unregistered_observation(&stopped, &lease).is_err());
    drop(lease);
    assert!(services.stop(stopped.epoch).is_err());
    let other = root.path().join("foreign");
    let _foreign = Store::create(&other, Origin::Headless, settings()).unwrap();
    services.state = other;
    assert!(services.stop(store.snapshot().unwrap().epoch).is_err());
    assert!(services.runner.state.borrow().calls.is_empty());
    assert_eq!(store.snapshot().unwrap().phase, Phase::StopRequested);
}

#[test]
fn native_service_invalid_receipt_never_becomes_owned_absence() {
    for cleanup in [false, true] {
        for fault in 0..7 {
            let root = storage::private_test_directory();
            let state = root.path().join("state");
            let store = Store::create(&state, Origin::Headless, settings()).unwrap();
            let provisioned = provision(&store);
            let services = services(root.path(), &store, Platform::Windows);
            let stage = trusted::fixture(root.path());
            services
                .install_reserved(&stage, provisioned.epoch)
                .unwrap();
            store.revoke().unwrap();
            let stopped = store.snapshot().unwrap();
            if cleanup {
                let observed = services.stop(stopped.epoch).unwrap();
                store
                    .confirm_stopped(observed.epoch, stopped.binding.as_ref().unwrap())
                    .unwrap();
                store.request_cleanup().unwrap();
            }
            let before = store.snapshot().unwrap();
            let mut receipt = services.registration().unwrap().1.unwrap();
            let directory = storage::private_directory(&state, false).unwrap();
            match fault {
                0 => storage::replace_private(&directory, Path::new(RECORD), b"{").unwrap(),
                1..=4 => {
                    match fault {
                        1 => receipt.installation = uuid::Uuid::new_v4().to_string(),
                        2 => receipt.origin = Origin::Desktop,
                        3 => receipt.epoch = before.epoch + 1,
                        4 => receipt.binding.account.push_str("-foreign"),
                        _ => unreachable!(),
                    }
                    storage::replace_private(
                        &directory,
                        Path::new(RECORD),
                        &serde_json::to_vec(&receipt).unwrap(),
                    )
                    .unwrap();
                }
                5 => {
                    std::fs::remove_file(state.join(RECORD)).unwrap();
                    storage::private_directory(&state.join(RECORD), true).unwrap();
                }
                6 => std::fs::remove_file(state.join(LOCK)).unwrap(),
                _ => unreachable!(),
            }
            services.runner.state.borrow_mut().calls.clear();
            let result = if cleanup {
                services.cleanup(before.epoch)
            } else {
                services.stop(before.epoch)
            };
            assert!(result.is_err(), "fault {fault}, cleanup {cleanup}");
            assert!(services.runner.state.borrow().calls.is_empty());
            assert_eq!(store.snapshot().unwrap(), before);
        }
    }
}

#[test]
#[cfg(unix)]
fn native_service_restart_requires_signed_provenance_without_adopting_cached_receipts() {
    let root = storage::private_test_directory();
    let store = Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
    let snapshot = provision(&store);
    let stage = trusted::fixture(root.path());
    let services = services(root.path(), &store, Platform::Linux);
    services.install(&stage, snapshot.epoch).unwrap();
    let record = services.registration().unwrap().1.unwrap();
    assert_eq!(record.provenance.as_deref(), Some(stage.provenance_path()));
    assert!(services.reopen_stage(snapshot.epoch).is_err());
    assert!(!services.runner.state.borrow().running);
    let mut legacy = record;
    legacy.provenance = None;
    services
        .with_record(|directory, _| {
            storage::replace_private(
                directory,
                Path::new(RECORD),
                &serde_json::to_vec(&legacy).unwrap(),
            )
        })
        .unwrap();
    assert_eq!(
        services
            .reopen_stage(snapshot.epoch)
            .err()
            .unwrap()
            .to_string(),
        "host.service_signed_provenance_required"
    );
    store.revoke().unwrap();
    assert!(services.reopen_stage(snapshot.epoch).is_err());
    assert!(services
        .registration()
        .unwrap()
        .1
        .unwrap()
        .provenance
        .is_none());
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
        if platform == Platform::Windows {
            assert_eq!(
                services
                    .install(&stage, snapshot.epoch)
                    .err()
                    .unwrap()
                    .to_string(),
                "host.service_windows_authenticated_graceful_stop_unavailable"
            );
            assert!(services.runner.state.borrow().calls.is_empty());
            continue;
        }
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

#[cfg(unix)]
#[test]
fn native_service_locked_vault_failed_start_and_foreign_definitions_fail_closed() {
    for platform in [Platform::Linux, Platform::Macos] {
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
        let path = &services.runner.definition;
        std::fs::write(path, b"foreign").unwrap();
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
    assert!(services.install_reserved(&stage, snapshot.epoch).is_err());
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
    assert!(active.install_reserved(&stage, snapshot.epoch).is_err());
    assert!(native::retain_operation().is_none());
    let reopened = Store::open(&root.path().join("state"), Origin::Headless).unwrap();
    reopened.revoke().unwrap();
    let other = services(root.path(), &reopened, Platform::Windows);
    assert_eq!(
        other.status().err().unwrap().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(active.runner.outstanding.borrow_mut().take());
    assert_eq!(active.status().unwrap().process, ProcessState::Stopped);
    assert!(active
        .registration()
        .unwrap()
        .1
        .unwrap()
        .native_snapshot
        .is_some());
    assert_eq!(reopened.snapshot().unwrap().phase, Phase::StopRequested);
}

#[test]
fn native_service_status_rejects_manager_overrides_ambiguous_properties_and_cross_origin() {
    let path = std::env::current_dir().unwrap().join("owned.service");
    let stopped = format!("Id=owned.service\nLoadState=loaded\nFragmentPath={}\nDropInPaths=\nActiveState=inactive\nSubState=dead\nMainPID=0\nExecMainPID=4321\nResult=success\nExecMainCode=2\nExecMainStatus=15\n", path.display());
    assert_eq!(
        definitions::linux_state(stopped.as_bytes(), &path).unwrap(),
        ProcessState::Stopped
    );
    assert_eq!(
        definitions::linux_state(
            stopped
                .replace("ExecMainStatus=15", "ExecMainStatus=78")
                .replace("Result=success", "Result=exit-code")
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
    services.install_reserved(&stage, snapshot.epoch).unwrap();
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

#[cfg(unix)]
#[test]
fn native_service_linux_liveness_uses_main_pid_result_and_retains_historical_exit() {
    let path = Path::new("/fixture/owned.service");
    let shape = |active: &str,
                 sub: &str,
                 main: u32,
                 historical: u32,
                 result: &str,
                 code: u32,
                 status: i32| {
        format!("Id=owned.service\nLoadState=loaded\nFragmentPath=/fixture/owned.service\nDropInPaths=\nActiveState={active}\nSubState={sub}\nMainPID={main}\nExecMainPID={historical}\nResult={result}\nExecMainCode={code}\nExecMainStatus={status}\n")
    };
    for (state, expected) in [
        (
            shape("inactive", "dead", 0, 0, "success", 0, 0),
            ProcessState::Stopped,
        ),
        (
            shape("active", "running", 123, 123, "success", 0, 0),
            ProcessState::Running,
        ),
        (
            shape("inactive", "dead", 0, 123, "success", 2, 15),
            ProcessState::Stopped,
        ),
        (
            shape("failed", "failed", 0, 123, "exit-code", 1, 23),
            ProcessState::Failed,
        ),
        (
            format!(
                "{}ControlGroup=/user.slice/owned/persistent-child\n",
                shape("inactive", "dead", 0, 123, "success", 2, 15)
            ),
            ProcessState::Stopped,
        ),
    ] {
        assert_eq!(
            definitions::linux_state(state.as_bytes(), path).unwrap(),
            expected
        );
    }
    assert!(definitions::linux_state(
        shape("inactive", "dead", 123, 123, "success", 2, 15).as_bytes(),
        path
    )
    .is_err());
}

#[cfg(unix)]
#[test]
fn native_service_linux_failed_cleanup_retries_removed_definition_and_resets_only_owned_unit() {
    let root = storage::private_test_directory();
    let store = Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
    let snapshot = provision(&store);
    let stage = trusted::fixture(root.path());
    let services = services(root.path(), &store, Platform::Linux);
    services.install(&stage, snapshot.epoch).unwrap();
    {
        let mut state = services.runner.state.borrow_mut();
        state.failed = true;
        state.fail_reload_once = true;
    }
    store.revoke().unwrap();
    let stopped = store.snapshot().unwrap();
    assert_eq!(
        services.stop(stopped.epoch).unwrap().process,
        ProcessState::Failed
    );
    store
        .confirm_stopped(stopped.epoch, stopped.binding.as_ref().unwrap())
        .unwrap();
    store.request_cleanup().unwrap();
    let epoch = store.snapshot().unwrap().epoch;
    assert!(services.cleanup(epoch).is_err());
    assert!(!services.runner.definition.exists());
    assert!(
        services
            .registration()
            .unwrap()
            .1
            .unwrap()
            .retiring_definition
    );
    assert_eq!(
        services.cleanup(epoch).unwrap().process,
        ProcessState::Absent
    );
    let record = services.registration().unwrap().1.unwrap();
    assert!(record.removed);
    assert!(record.definition.contains("KillMode=process\n"));
    assert!(record.definition.contains("SendSIGKILL=no\n"));
    assert!(record.definition.contains("TimeoutStopSec=10s\n"));
    let state = services.runner.state.borrow();
    assert_eq!(
        state
            .calls
            .iter()
            .filter(|request| matches!(request,
        NativeRequest::Process { args, .. } if args.iter().any(|arg| arg == "disable")))
            .count(),
        1
    );
    let resets: Vec<_> = state
        .calls
        .iter()
        .filter_map(|request| match request {
            NativeRequest::Process { args, .. } if args.iter().any(|arg| arg == "reset-failed") => {
                Some(args)
            }
            _ => None,
        })
        .collect();
    assert_eq!(resets.len(), 1);
    assert_eq!(
        resets[0],
        &vec![
            std::ffi::OsString::from("--user"),
            "reset-failed".into(),
            record.resource.into()
        ]
    );
    assert!(stage.path().exists());
}

#[cfg(unix)]
#[test]
fn native_service_start_reconciles_bounded_transitions_without_duplicate_start() {
    let root = storage::private_test_directory();
    let store = Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
    let snapshot = provision(&store);
    let stage = trusted::fixture(root.path());
    let services = services(root.path(), &store, Platform::Linux);
    services.install(&stage, snapshot.epoch).unwrap();
    services.runner.state.borrow_mut().start_delay_queries = 2;
    assert_eq!(
        services
            .start_with(&stage, snapshot.epoch, || Ok(()))
            .unwrap()
            .process,
        ProcessState::Running
    );
    {
        let mut state = services.runner.state.borrow_mut();
        state.running = false;
        state.start_delay_queries = usize::MAX;
    }
    assert_eq!(
        services
            .start_with(&stage, snapshot.epoch, || Ok(()))
            .unwrap()
            .process,
        ProcessState::Transitioning
    );
    assert_eq!(
        services
            .start_with(&stage, snapshot.epoch, || Ok(()))
            .unwrap()
            .process,
        ProcessState::Transitioning
    );
    assert_eq!(
        services
            .runner
            .state
            .borrow()
            .calls
            .iter()
            .filter(|request| matches!(request,
        NativeRequest::Process { args, .. } if args.iter().any(|arg| arg == "start")))
            .count(),
        2
    );
    struct RevokeOnObservation<'a>(&'a FakeRunner, &'a Store);
    impl CommandRunner for RevokeOnObservation<'_> {
        fn execute(&self, request: &NativeRequest) -> io::Result<NativeOutput> {
            let output = self.0.execute(request)?;
            self.1.revoke()?;
            Ok(output)
        }
    }
    let revoked = Services {
        store: &store,
        state: services.state.clone(),
        context: services.context.clone(),
        runner: RevokeOnObservation(&services.runner, &store),
    };
    assert_eq!(
        revoked
            .start_with(&stage, snapshot.epoch, || Ok(()))
            .err()
            .unwrap()
            .to_string(),
        "host.service_stale_or_unapproved_operation"
    );
    assert_eq!(store.snapshot().unwrap().phase, Phase::StopRequested);
}

#[test]
fn native_service_windows_recovery_is_reserved_exact_and_stop_has_no_destructive_fallback() {
    let root = storage::private_test_directory();
    let store = Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
    let snapshot = provision(&store);
    let stage = trusted::fixture(root.path());
    let services = services(root.path(), &store, Platform::Windows);
    assert!(services.install(&stage, snapshot.epoch).is_err());
    assert!(services
        .start_with(&stage, snapshot.epoch, || panic!(
            "disabled Windows start must not invoke credentials"
        ))
        .is_err());
    assert!(services.runner.state.borrow().calls.is_empty());
    services.install_reserved(&stage, snapshot.epoch).unwrap();
    let mut record = services.registration().unwrap().1.unwrap();
    assert!(record
        .definition
        .contains(&format!("fesTerm-host-reservation:{}", record.reservation)));
    record.native_snapshot = None;
    record.native_submitted = false;
    services
        .save(&record, snapshot.epoch, Phase::Provisioned)
        .unwrap();
    assert_eq!(
        services.status().err().unwrap().to_string(),
        "host.service_foreign_native_definition"
    );
    record.native_submitted = true;
    services
        .save(&record, snapshot.epoch, Phase::Provisioned)
        .unwrap();
    services.runner.state.borrow_mut().native_xml = Some("foreign".into());
    assert!(services.status().is_err());
    services.runner.state.borrow_mut().native_xml = record.native_expected.clone();
    services.runner.state.borrow_mut().native_security = Some("foreign".into());
    assert!(services.status().is_err());
    services.runner.state.borrow_mut().native_security = None;
    assert_eq!(services.status().unwrap().process, ProcessState::Stopped);
    assert!(services
        .registration()
        .unwrap()
        .1
        .unwrap()
        .native_snapshot
        .is_some());
    {
        let mut uncertain = services.registration().unwrap().1.unwrap();
        uncertain.native_snapshot = None;
        services
            .save(&uncertain, snapshot.epoch, Phase::Provisioned)
            .unwrap();
        services.runner.state.borrow_mut().native_xml = None;
        assert_eq!(
            services.status().err().unwrap().to_string(),
            "host.service_task_registration_outcome_unknown"
        );
        assert!(services.install_reserved(&stage, snapshot.epoch).is_err());
        services.runner.state.borrow_mut().native_xml = uncertain.native_expected;
    }
    services.runner.state.borrow_mut().running = true;
    store.revoke().unwrap();
    assert!(services.stop(store.snapshot().unwrap().epoch).is_err());
    assert!(!services
        .runner
        .state
        .borrow()
        .calls
        .iter()
        .any(|request| matches!(
            request,
            NativeRequest::Task {
                action: TaskAction::Stop,
                ..
            }
        )));
    assert_eq!(store.snapshot().unwrap().phase, Phase::StopRequested);
}
