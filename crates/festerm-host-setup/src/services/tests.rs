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
    receipt: PathBuf,
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
    native_prepared_upgrade: Option<(String, String)>,
    replace_receipt_on_prepare: Option<Registration>,
    failed: bool,
    fail_reload_once: bool,
    pending_start: bool,
    start_delay_queries: usize,
    quiesce_after_query: bool,
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
                if (args.contains(&"start")
                    || args.contains(&"kickstart")
                    || (args.contains(&"enable") && args.contains(&"--now")))
                    && !state.refuse_start
                {
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
                        let record: Registration =
                            serde_json::from_slice(&std::fs::read(&self.receipt).unwrap()).unwrap();
                        output.stdout = format!(
                            "path = {}\nprogram = {}\narguments = {{\n{}\n{}\n--state-dir\n{}\n}}\nstate = {}\n{}",
                            self.definition.display(), record.executable.display(), record.executable.display(),
                            definitions::record_runner_verb(&record),
                            record.state.display(), if state.running { "running" } else { "waiting" },
                            if state.running { "pid = 123\n" } else { "" }).into_bytes();
                    }
                }
            }
            NativeRequest::Task { action, .. } => {
                match action {
                    TaskAction::Prepare(xml) => {
                        let canonical = match &state.native_prepared_upgrade {
                            Some((legacy, current)) if legacy == xml => current.clone(),
                            _ if xml.starts_with("normalized:") => xml.clone(),
                            _ => format!("normalized:{xml}"),
                        };
                        if let Some(replacement) = state.replace_receipt_on_prepare.take() {
                            let directory =
                                storage::private_directory(self.receipt.parent().unwrap(), false)?;
                            storage::replace_private(
                                &directory,
                                Path::new(RECORD),
                                &serde_json::to_vec(&replacement).unwrap(),
                            )?;
                        }
                        output.stdout = serde_json::to_vec(&TaskSnapshot {
                            xml: Some(canonical),
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
                if matches!(action, TaskAction::Query) && state.quiesce_after_query {
                    state.running = false;
                }
                output.stdout = serde_json::to_vec(&TaskSnapshot {
                    xml: state.native_xml.clone(),
                    security: state.native_xml.as_ref().map(|_| {
                        state.native_security.clone().unwrap_or_else(|| {
                            "task-owner-system-full-control-v1:S-1-5-21-1-2-3-1001".into()
                        })
                    }),
                    state: if state.observation_error {
                        0
                    } else if state.pending_start {
                        2
                    } else if state.running {
                        4
                    } else {
                        3
                    },
                    last_exit: if state.failed { 1 } else { 0 },
                })
                .unwrap();
            }
        }
        Ok(output)
    }
}

fn settings() -> Settings {
    Settings {
        filesystem: None,
        ordinary_access: Default::default(),
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
            receipt: root.join("state").join(RECORD),
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

#[cfg(all(feature = "cloud", feature = "host-runtime", unix))]
fn reconfigure_lifecycle(store: &Store, allow_creation: bool) -> Snapshot {
    reconfigure_lifecycle_mode(
        store,
        allow_creation,
        store.snapshot().unwrap().settings.ordinary_access,
    )
}

#[cfg(all(feature = "cloud", feature = "host-runtime", unix))]
fn reconfigure_lifecycle_mode(
    store: &Store,
    allow_creation: bool,
    ordinary_access: crate::lifecycle::OrdinaryAccess,
) -> Snapshot {
    store.revoke().unwrap();
    let stopped = store.snapshot().unwrap();
    store
        .confirm_stopped(stopped.epoch, stopped.binding.as_ref().unwrap())
        .unwrap();
    let before = store.snapshot().unwrap();
    let mut settings = before.settings.clone();
    settings.allow_creation = allow_creation;
    settings.ordinary_access = ordinary_access;
    let ticket = store.request_enable_if_current(&before, &settings).unwrap();
    store
        .record_provisioned(&ticket, before.binding.unwrap())
        .unwrap();
    store.snapshot().unwrap()
}

#[cfg(all(feature = "cloud", feature = "host-runtime", unix))]
#[test]
fn native_service_stopped_creation_projection_preserves_registration_and_durable_state() {
    use crate::enrollment::OwnerStorage;
    let (root, store, _vault) = crate::enrollment::tests::setup_origin(Origin::Headless);
    let owners = store
        .owner_enrollment_with_storage(OwnerStorage::HeadlessProtectedFile)
        .unwrap();
    owners
        .initialize(Path::new("host-identity"), "github:1234".into())
        .unwrap();
    let mut services = services(root.path(), &store, Platform::Linux);
    services.state = root.path().join("installation");
    services.runner.receipt = services.state.join(RECORD);
    let stage = trusted::fixture(root.path());
    services
        .install_with_recovery(&stage, store.snapshot().unwrap().epoch)
        .unwrap();
    let original = services.registration().unwrap().1.unwrap();
    let definition = std::fs::read(&services.runner.definition).unwrap();
    let durable = services
        .state
        .join("fixture-independent-daemon-and-catalog");
    std::fs::write(&durable, b"same daemon, shells, catalog").unwrap();
    // Simulate Settings independently publishing the identical verified release
    // under its own cache. The registered executable must never move there.
    let selected_root = storage::private_test_directory();
    let selected = trusted::fixture(selected_root.path());
    for enabled in [true, false] {
        let mode = if enabled {
            crate::lifecycle::OrdinaryAccess::ProviderOwner
        } else {
            crate::lifecycle::OrdinaryAccess::OwnerOnly
        };
        let expected = reconfigure_lifecycle_mode(&store, enabled, mode);
        let owner = owners.snapshot().unwrap();
        let old = services.registration().unwrap().1.unwrap();
        assert_ne!(old.settings, expected.settings);
        // The original generic install entry point intentionally stays strict;
        // only the explicit reviewed setup completion can project this flag.
        assert_eq!(
            services
                .install_with_recovery(&stage, expected.epoch)
                .err()
                .unwrap()
                .to_string(),
            "host.service_explicit_replacement_required"
        );
        // An interruption here leaves both executable entry points fail closed.
        assert!(services.reopen_stage(expected.epoch).is_err());
        assert!(services
            .start_with(&stage, expected.epoch, || Ok(()))
            .is_err());
        let calls_before = services.runner.state.borrow().calls.len();
        let complete = || {
            services.complete_setup_with(
                &selected,
                &expected,
                &owner,
                &owners,
                &tokio_util::sync::CancellationToken::new(),
                |_| Ok(trusted::fixture(root.path())),
            )
        };
        assert_eq!(complete().unwrap().process, ProcessState::Stopped);
        let committed = services.registration().unwrap().1.unwrap();
        let mut wanted = original.clone();
        wanted.settings.allow_creation = enabled;
        wanted.settings.ordinary_access = mode;
        assert_eq!(committed, wanted);
        assert_eq!(
            std::fs::read(&services.runner.definition).unwrap(),
            definition
        );
        assert_eq!(
            std::fs::read(&durable).unwrap(),
            b"same daemon, shells, catalog"
        );
        assert_eq!(owners.snapshot().unwrap(), owner);
        // Crash after the atomic replace but before its reply: another explicit
        // Enable completes with the same bytes and no manager mutation.
        assert_eq!(complete().unwrap().process, ProcessState::Stopped);
        assert_eq!(services.registration().unwrap().1.unwrap(), committed);
        assert!(services.runner.state.borrow().calls[calls_before..].iter().all(|call|
            matches!(call, NativeRequest::Process { args, .. } if args.iter().any(|arg| arg == "show"))));
    }
}

#[cfg(all(feature = "cloud", feature = "host-runtime", unix))]
#[test]
fn native_service_stopped_creation_projection_rejects_scope_policy_and_native_races() {
    use crate::enrollment::OwnerStorage;
    // Every case retains the exact old receipt, definition and lifecycle. A
    // real held runtime lease exercises exclusion rather than PID inference.
    for case in [
        "running",
        "unknown",
        "runtime",
        "failed",
        "absent",
        "orphan-runtime",
        "startup",
        "friendly",
        "owner",
        "scope",
        "path",
        "digest",
        "version",
        "bundle",
    ] {
        let (root, store, _vault) = crate::enrollment::tests::setup_origin(Origin::Headless);
        let owners = store
            .owner_enrollment_with_storage(OwnerStorage::HeadlessProtectedFile)
            .unwrap();
        owners
            .initialize(Path::new("host-identity"), "github:1234".into())
            .unwrap();
        let mut services = services(root.path(), &store, Platform::Linux);
        services.state = root.path().join("installation");
        services.runner.receipt = services.state.join(RECORD);
        let stage = trusted::fixture(root.path());
        services
            .install_with_recovery(&stage, store.snapshot().unwrap().epoch)
            .unwrap();
        let mut expected = reconfigure_lifecycle(&store, true);
        let mut owner = owners.snapshot().unwrap();
        let old = std::fs::read(&services.runner.receipt).unwrap();
        let mut selected = trusted::fixture(root.path());
        let mut registered = trusted::fixture(root.path());
        let mut runtime = None;
        match case {
            "running" => services.runner.state.borrow_mut().running = true,
            "unknown" => services.runner.state.borrow_mut().observation_error = true,
            "failed" => services.runner.state.borrow_mut().failed = true,
            "absent" => services.runner.state.borrow_mut().registered = false,
            "orphan-runtime" => {
                let directory = storage::private_directory(&services.state, false).unwrap();
                storage::write_new_private(
                    &directory,
                    Path::new("host-runtime.json"),
                    b"preserve orphan runtime evidence",
                )
                .unwrap();
            }
            "runtime" => {
                let directory = storage::private_directory(&services.state, false).unwrap();
                runtime = Some(
                    Lease::exclusive(
                        storage::open_private_file(
                            &directory,
                            Path::new("host-runtime.lock"),
                            true,
                        )
                        .unwrap(),
                    )
                    .unwrap(),
                );
            }
            "startup" | "friendly" => {
                store.revoke().unwrap();
                let stopped = store.snapshot().unwrap();
                store
                    .confirm_stopped(stopped.epoch, stopped.binding.as_ref().unwrap())
                    .unwrap();
                let before = store.snapshot().unwrap();
                let mut settings = before.settings.clone();
                if case == "startup" {
                    settings.startup = Startup::UserSignIn;
                } else {
                    settings.friendly_name.push_str(" changed");
                }
                let ticket = store.request_enable_if_current(&before, &settings).unwrap();
                store
                    .record_provisioned(&ticket, before.binding.unwrap())
                    .unwrap();
                expected = store.snapshot().unwrap();
                owner = owners.snapshot().unwrap();
            }
            "owner" => owner.remote_access = !owner.remote_access,
            "scope" => expected.settings.friendly_name.push_str(" stale"),
            "path" => registered.path = root.path().join("unregistered"),
            "digest" => registered.digest = "0".repeat(64),
            "version" => selected = trusted::version_fixture(root.path(), "0.15.0"),
            "bundle" => {
                selected = trusted::version_fixture_with_sessiond(
                    root.path(),
                    crate::VERSION,
                    b"different authenticated helper",
                )
            }
            _ => unreachable!(),
        }
        let before = store.snapshot().unwrap();
        assert!(
            services
                .complete_setup_with(
                    &selected,
                    &expected,
                    &owner,
                    &owners,
                    &tokio_util::sync::CancellationToken::new(),
                    |_| Ok(registered)
                )
                .is_err(),
            "{case}"
        );
        assert_eq!(
            std::fs::read(&services.runner.receipt).unwrap(),
            old,
            "{case}"
        );
        assert_eq!(store.snapshot().unwrap(), before, "{case}");
        drop(runtime);
    }
}

#[cfg(all(feature = "cloud", feature = "host-runtime", unix))]
#[test]
fn native_service_stopped_creation_projection_rechecks_after_qualification() {
    use crate::enrollment::OwnerStorage;
    use festerm_tunnel_bridge::owner_auth::OwnerSigningKey;
    for case in [
        "owner-revision",
        "revoke-key",
        "binding",
        "epoch",
        "record",
        "cancel",
    ] {
        let (root, store, _vault) = crate::enrollment::tests::setup_origin(Origin::Headless);
        let owners = store
            .owner_enrollment_with_storage(OwnerStorage::HeadlessProtectedFile)
            .unwrap();
        let initial = owners
            .initialize(Path::new("host-identity"), "github:1234".into())
            .unwrap();
        let key = OwnerSigningKey::from_seed(&[41; 32]).public_key();
        owners
            .enroll(
                initial.revision,
                key,
                &crate::enrollment::owner_key_fingerprint(&key).unwrap(),
            )
            .unwrap();
        let mut services = services(root.path(), &store, Platform::Linux);
        services.state = root.path().join("installation");
        services.runner.receipt = services.state.join(RECORD);
        let stage = trusted::fixture(root.path());
        services
            .install_with_recovery(&stage, store.snapshot().unwrap().epoch)
            .unwrap();
        let mut expected = reconfigure_lifecycle(&store, true);
        let owner = owners.snapshot().unwrap();
        let before = services.registration().unwrap().1.unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        if case == "binding" {
            expected
                .binding
                .as_mut()
                .unwrap()
                .account
                .push_str(" stale");
        }
        let result =
            services.complete_setup_with(&stage, &expected, &owner, &owners, &cancel, |_| {
                match case {
                    "owner-revision" => {
                        owners
                            .approve_permissions(owner.revision, false, owner.capabilities)
                            .unwrap();
                    }
                    "revoke-key" => {
                        owners.revoke(owner.revision, key).unwrap();
                    }
                    "cancel" => cancel.cancel(),
                    "epoch" => {
                        store.revoke().unwrap();
                    }
                    "record" => {
                        let mut changed = before.clone();
                        changed.settings.allow_creation = true;
                        let directory = storage::private_directory(&services.state, false).unwrap();
                        storage::replace_private(
                            &directory,
                            Path::new(RECORD),
                            &serde_json::to_vec(&changed).unwrap(),
                        )
                        .unwrap();
                    }
                    _ => panic!("a foreign binding must be rejected before qualification"),
                }
                Ok(trusted::fixture(root.path()))
            });
        assert!(result.is_err(), "{case}");
        let after = services.registration().unwrap().1.unwrap();
        let mut wanted = before;
        if case == "record" {
            wanted.settings.allow_creation = true;
        }
        assert_eq!(after, wanted, "{case}");
    }
}

#[test]
fn host_process_recovery_native_policy_is_bounded_and_origin_exact() {
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        #[cfg(windows)]
        if platform != Platform::Windows {
            continue;
        }
        if platform == Platform::Windows && !cfg!(all(feature = "cloud", feature = "host-runtime"))
        {
            continue;
        }
        for startup in [Startup::Manual, Startup::UserSignIn] {
            for origin in [Origin::Headless, Origin::Desktop] {
                let root = storage::private_test_directory();
                let mut options = settings();
                options.startup = startup;
                let store = Store::create(&root.path().join("state"), origin, options).unwrap();
                let snapshot = provision(&store);
                let mut manager = services(root.path(), &store, platform);
                if platform == Platform::Macos && startup == Startup::Manual {
                    manager.runner.definition = root
                        .path()
                        .join("state/native-manual-services")
                        .join(manager.runner.definition.file_name().unwrap());
                }
                let worker = platform == Platform::Windows && origin == Origin::Desktop;
                let stage = if worker {
                    trusted::windows_fixture(root.path(), crate::VERSION)
                } else {
                    trusted::fixture(root.path())
                };
                manager
                    .install_with_recovery(&stage, snapshot.epoch)
                    .unwrap();
                if worker {
                    assert!(
                        manager.require_recovery_registration().is_err(),
                        "test executable is not the registered GUI worker"
                    );
                } else {
                    manager.require_recovery_registration().unwrap();
                }
                let record = manager.registration().unwrap().1.unwrap();
                assert_eq!(record.version, if worker { 6 } else { 4 });
                assert_eq!(
                    definitions::record_runner_verb(&record),
                    if origin == Origin::Desktop {
                        "run-desktop-managed"
                    } else {
                        "run-managed"
                    }
                );
                match platform {
                    Platform::Linux => {
                        assert!(record
                            .definition
                            .contains("Restart=on-failure\nRestartSec=60s"));
                        assert!(record.definition.contains("KillMode=process\n"));
                        assert!(record.definition.contains("SendSIGKILL=no\n"));
                        if startup == Startup::Manual {
                            assert!(record.definition.contains("StartLimitBurst=3\n"));
                            assert!(!record.definition.contains("RestartForceExitStatus="));
                        } else {
                            assert!(record.definition.contains("StartLimitIntervalSec=0\n"));
                            assert!(record.definition.contains("RestartForceExitStatus=75\n"));
                        }
                    }
                    Platform::Macos => {
                        assert!(record
                            .definition
                            .contains("<key>SuccessfulExit</key><false/>"));
                        assert!(record
                            .definition
                            .contains("<key>ThrottleInterval</key><integer>60</integer>"));
                        if startup == Startup::Manual {
                            assert!(manager
                                .runner
                                .definition
                                .starts_with(root.path().join("state")));
                            assert!(!manager.context.registration.exists());
                        } else {
                            assert!(manager
                                .runner
                                .definition
                                .starts_with(&manager.context.registration));
                        }
                    }
                    Platform::Windows => {
                        assert!(record.definition.contains("<RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure>"));
                        assert!(record
                            .definition
                            .contains("<LogonType>InteractiveToken</LogonType>"));
                        assert!(record
                            .definition
                            .contains("<RunLevel>LeastPrivilege</RunLevel>"));
                        assert_eq!(
                            record.definition.contains("<LogonTrigger>"),
                            startup == Startup::UserSignIn
                        );
                    }
                }
                assert_eq!(store.snapshot().unwrap(), snapshot);
            }
        }
    }
}

#[test]
#[cfg(unix)]
fn host_process_recovery_manual_macos_bootstraps_only_explicitly_and_boots_out_on_stop() {
    let root = storage::private_test_directory();
    let state = root.path().join("state");
    let store = Store::create(&state, Origin::Headless, settings()).unwrap();
    let snapshot = provision(&store);
    let mut manager = services(root.path(), &store, Platform::Macos);
    manager.runner.definition = state
        .join("native-manual-services")
        .join(manager.runner.definition.file_name().unwrap());
    let stage = trusted::fixture(root.path());
    manager
        .install_with_recovery(&stage, snapshot.epoch)
        .unwrap();
    assert!(!manager.runner.state.borrow().registered);
    assert!(!manager.runner.state.borrow().running);
    manager
        .start_with(&stage, snapshot.epoch, || Ok(()))
        .unwrap();
    assert!(manager.runner.state.borrow().running);
    store.revoke().unwrap();
    let stopped = store.snapshot().unwrap();
    manager.stop(stopped.epoch).unwrap();
    assert!(!manager.runner.state.borrow().registered);
    assert!(!manager.runner.state.borrow().running);
    store
        .confirm_stopped(stopped.epoch, stopped.binding.as_ref().unwrap())
        .unwrap();
    store.request_cleanup().unwrap();
    manager.cleanup(store.snapshot().unwrap().epoch).unwrap();
    assert!(!manager.runner.definition.exists());
    assert!(!manager.context.registration.exists());
}

#[test]
fn host_process_recovery_existing_receipts_and_missing_legacy_files_keep_exact_policy() {
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        #[cfg(windows)]
        if platform != Platform::Windows {
            continue;
        }
        if platform == Platform::Windows && !cfg!(all(feature = "cloud", feature = "host-runtime"))
        {
            continue;
        }
        let root = storage::private_test_directory();
        let store =
            Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
        let snapshot = provision(&store);
        let manager = services(root.path(), &store, platform);
        let stage = trusted::fixture(root.path());
        manager.install(&stage, snapshot.epoch).unwrap();
        let original = manager.registration().unwrap().1.unwrap();
        manager
            .install_with_recovery(&stage, snapshot.epoch)
            .unwrap();
        assert_eq!(manager.registration().unwrap().1.unwrap(), original);
        assert!(manager.require_recovery_registration().is_err());
        assert_eq!(original.version, 2);
        assert!(!original.definition.contains("run-managed"));
        assert!(!original.definition.contains("RestartOnFailure"));
        assert!(!original.definition.contains("SuccessfulExit"));
        if platform != Platform::Windows {
            std::fs::remove_file(&manager.runner.definition).unwrap();
            manager
                .install_with_recovery(&stage, snapshot.epoch)
                .unwrap();
            assert_eq!(
                std::fs::read(&manager.runner.definition).unwrap(),
                original.definition.as_bytes()
            );
            assert_eq!(manager.registration().unwrap().1.unwrap(), original);
        }
    }
}

#[test]
#[cfg(unix)]
fn host_process_recovery_older_bundle_cannot_create_new_restart_authority() {
    let root = storage::private_test_directory();
    let store = Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
    let snapshot = provision(&store);
    let manager = services(root.path(), &store, Platform::Linux);
    let old = trusted::version_fixture(root.path(), "0.15.0");
    assert!(manager.install_with_recovery(&old, snapshot.epoch).is_err());
    assert!(manager.registration().unwrap().1.is_none());
    assert!(manager.runner.state.borrow().calls.is_empty());
}

fn mac_observation_path(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join("festerm-macos-observation-fixture");
    assert!(root.is_absolute());
    root.join(name)
}

fn mac_observation_record() -> Registration {
    Registration {
        version: 4,
        installation: uuid::Uuid::new_v4().to_string(),
        origin: Origin::Desktop,
        binding: binding(),
        epoch: 2,
        settings: settings(),
        platform: Platform::Macos,
        user: "501".into(),
        resource: "fixture-service".into(),
        executable: mac_observation_path("host executable"),
        digest: "00".repeat(32),
        windows_managed: None,
        provenance: None,
        state: mac_observation_path("state directory"),
        definition: String::new(),
        reservation: uuid::Uuid::new_v4().to_string(),
        native_expected: None,
        native_submitted: false,
        native_snapshot: None,
        native_security: None,
        retiring_definition: false,
        removed: false,
    }
}

fn mac_observation_text(record: &Registration, fields: &str) -> String {
    format!(
        "gui/501/fixture-service = {{\n\tpath = {}\n\tprogram = {}\n\targuments = {{\n\t\t{}\n\t\t{}\n\t\t--state-dir\n\t\t{}\n\t}}\n{fields}}}\n",
        mac_observation_path("service.plist").display(), record.executable.display(), record.executable.display(),
        definitions::record_runner_verb(record), record.state.display(),
    )
}

#[test]
fn native_service_macos_observation_ignores_nested_coalition_fields() {
    let record = mac_observation_record();
    // Captured native shape: direct state is tab-indented; resource/jetsam
    // coalition states are double-tab-indented within their own dictionaries.
    let nested = "\tresource coalition = {\n\t\tstate = active\n\t\tpid = 999\n\t\tlast exit code = 19\n\t}\n\tjetsam coalition = {\n\t\tstate = active\n\t\tpid = 998\n\t}\n";
    for (direct, expected) in [
        (
            "\tstate = not running\n\tlast exit code = 0\n",
            ProcessState::Stopped,
        ),
        ("\tstate = running\n\tpid = 123\n", ProcessState::Running),
        (
            "\tstate = exited\n\tlast exit code = 17\n",
            ProcessState::Failed,
        ),
        ("\tstate = spawn scheduled\n", ProcessState::Transitioning),
    ] {
        let text = mac_observation_text(&record, &format!("{direct}{nested}"));
        assert_eq!(
            definitions::mac_state(
                text.as_bytes(),
                &mac_observation_path("service.plist"),
                &record
            )
            .unwrap(),
            expected
        );
    }
}

#[test]
fn native_service_macos_observation_rejects_duplicate_direct_fields() {
    let record = mac_observation_record();
    let direct = "\tstate = running\n\tpid = 123\n\tlast exit code = 0\n";
    assert_eq!(
        definitions::mac_state(
            mac_observation_text(&record, direct).as_bytes(),
            &mac_observation_path("service.plist"),
            &record
        )
        .unwrap(),
        ProcessState::Running
    );
    let duplicate_path = format!(
        "\tpath = {}\n",
        mac_observation_path("service.plist").display()
    );
    let foreign_path = format!(
        "\tpath = {}\n",
        mac_observation_path("foreign-service.plist").display()
    );
    let foreign_program = format!(
        "\tprogram = {}\n",
        mac_observation_path("foreign-program").display()
    );
    let duplicate_program = format!("\tprogram = {}\n", record.executable.display());
    let duplicate_arguments = format!(
        "\targuments = {{\n\t\t{}\n\t\t{}\n\t\t--state-dir\n\t\t{}\n\t}}\n",
        record.executable.display(),
        definitions::record_runner_verb(&record),
        record.state.display()
    );
    for duplicate in [
        "\tstate = running\n",
        "\tstate = not running\n",
        "\tpid = 123\n",
        "\tpid = 456\n",
        "\tlast exit code = 0\n",
        "\tlast exit code = 17\n",
        duplicate_path.as_str(),
        foreign_path.as_str(),
        foreign_program.as_str(),
        duplicate_program.as_str(),
        duplicate_arguments.as_str(),
    ] {
        let text = mac_observation_text(&record, &format!("{direct}{duplicate}"));
        assert!(
            definitions::mac_state(
                text.as_bytes(),
                &mac_observation_path("service.plist"),
                &record
            )
            .is_err(),
            "duplicate accepted: {duplicate}"
        );
    }
}

#[test]
fn native_service_macos_observation_rejects_wrong_identity_and_malformed_nesting() {
    let record = mac_observation_record();
    let text = mac_observation_text(&record, "\tstate = not running\n\tlast exit code = 0\n");
    assert_eq!(
        definitions::mac_state(
            text.as_bytes(),
            &mac_observation_path("service.plist"),
            &record
        )
        .unwrap(),
        ProcessState::Stopped
    );
    let nested_identity = format!("gui/501/fixture-service = {{\n\tstate = not running\n\tforeign = {{\n\t\tpath = {}\n\t\tprogram = {}\n\t\targuments = {{\n\t\t\t{}\n\t\t\t{}\n\t\t\t--state-dir\n\t\t\t{}\n\t\t}}\n\t}}\n}}\n", mac_observation_path("service.plist").display(), record.executable.display(), record.executable.display(), definitions::record_runner_verb(&record), record.state.display());
    for invalid in [
        text.replace(
            &format!("path = {}", mac_observation_path("service.plist").display()),
            &format!(
                "path = {}",
                mac_observation_path("foreign-service.plist").display()
            ),
        ),
        text.replace(
            &format!("program = {}", record.executable.display()),
            &format!(
                "program = {}",
                mac_observation_path("foreign-program").display()
            ),
        ),
        text.replace("\t\trun-desktop-managed\n", "\t\trun-managed\n"),
        text.trim_end().trim_end_matches('}').to_owned(),
        format!("{text}}}\n"),
        format!("{text}state = running\n"),
        text.replace("\tlast exit code = 0", "\t\tlast exit code = 0"),
        text.replace("\t}\n", "}\n"),
        nested_identity,
        mac_observation_text(
            &record,
            "\tstate = running\n\tcoalition = {\n\t\tpid = 123\n\t}\n",
        ),
        text.replace("\n}\n", "\n\tcoalition = {\n\t\tstate = active\n}\n"),
    ] {
        assert!(
            definitions::mac_state(
                invalid.as_bytes(),
                &mac_observation_path("service.plist"),
                &record
            )
            .is_err(),
            "malformed observation accepted"
        );
    }
}

#[test]
fn native_service_macos_observation_preserves_flat_fixture_format() {
    let record = mac_observation_record();
    let flat = format!(
        "path = {}\nprogram = {}\narguments = {{\n{}\n{}\n--state-dir\n{}\n}}\nstate = waiting\n",
        mac_observation_path("service.plist").display(),
        record.executable.display(),
        record.executable.display(),
        definitions::record_runner_verb(&record),
        record.state.display()
    );
    assert_eq!(
        definitions::mac_state(
            flat.as_bytes(),
            &mac_observation_path("service.plist"),
            &record
        )
        .unwrap(),
        ProcessState::Stopped
    );
    let mut literal = record;
    literal.executable = mac_observation_path("host = {");
    literal.state = mac_observation_path("state {");
    let framed = mac_observation_text(&literal, "\tstate = not running\n\tlast exit code = 0\n");
    assert_eq!(
        definitions::mac_state(
            framed.as_bytes(),
            &mac_observation_path("service.plist"),
            &literal
        )
        .unwrap(),
        ProcessState::Stopped
    );
}

#[test]
fn native_service_desktop_runner_definition_is_origin_exact_without_headless_adoption() {
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        #[cfg(windows)]
        if platform != Platform::Windows {
            continue;
        }
        let root = storage::private_test_directory();
        let store = Store::create(&root.path().join("state"), Origin::Desktop, settings()).unwrap();
        let snapshot = provision(&store);
        let manager = services(root.path(), &store, platform);
        let stage = trusted::fixture(root.path());
        if platform == Platform::Windows && !cfg!(all(feature = "cloud", feature = "host-runtime"))
        {
            assert_eq!(
                manager
                    .install(&stage, snapshot.epoch)
                    .err()
                    .unwrap()
                    .to_string(),
                "host.service_windows_authenticated_graceful_stop_unavailable"
            );
            assert!(manager.registration().unwrap().1.is_none());
            assert!(manager.runner.state.borrow().calls.is_empty());
            assert_eq!(store.snapshot().unwrap(), snapshot);
            continue;
        }
        manager.install(&stage, snapshot.epoch).unwrap();
        let (_, record) = manager.registration().unwrap();
        let mut record = record.unwrap();
        assert!(record.definition.contains("run-desktop"));
        assert_eq!(definitions::runner_verb(record.origin), "run-desktop");
        if platform == Platform::Macos {
            let path = definitions::definition_path(&manager.context, &record);
            let old = format!(
                "path = {}\nprogram = {}\narguments = {{\n{}\nrun\n--state-dir\n{}\n}}\nstate = waiting\n",
                path.display(), record.executable.display(),
                record.executable.display(), record.state.display(),
            );
            assert!(definitions::mac_state(old.as_bytes(), &path, &record).is_err());
            let exact = old.replace("\nrun\n", "\nrun-desktop\n");
            assert_eq!(
                definitions::mac_state(exact.as_bytes(), &path, &record).unwrap(),
                ProcessState::Stopped
            );
        }
        record.origin = Origin::Headless;
        assert!(!definitions::render(&record)
            .unwrap()
            .contains("run-desktop"));
        assert_eq!(definitions::runner_verb(record.origin), "run");
        assert_eq!(store.snapshot().unwrap(), snapshot);
    }
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
            if cfg!(all(feature = "cloud", feature = "host-runtime")) {
                services.install(&stage, snapshot.epoch).unwrap();
            } else {
                assert_eq!(
                    services
                        .install(&stage, snapshot.epoch)
                        .err()
                        .unwrap()
                        .to_string(),
                    "host.service_windows_authenticated_graceful_stop_unavailable"
                );
                assert!(services.runner.state.borrow().calls.is_empty());
            }
            continue;
        }
        assert_eq!(
            services.status().err().unwrap().to_string(),
            "host.service_not_installed_no_native_observation"
        );
        let installed = services.install(&stage, snapshot.epoch).unwrap();
        if platform != Platform::Linux || startup != Startup::UserSignIn {
            assert_ne!(installed.process, ProcessState::Running);
        }
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
fn native_service_linux_sign_in_activation_is_owned_and_manual_remains_inert() {
    for startup in [Startup::Manual, Startup::UserSignIn] {
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
        let services = services(root.path(), &store, Platform::Linux);
        services.install(&stage, snapshot.epoch).unwrap();
        let record = services.registration().unwrap().1.unwrap();
        assert_eq!(store.snapshot().unwrap(), snapshot);
        assert!(record.definition.contains("KillMode=process\n"));
        assert!(record.definition.contains("SendSIGKILL=no\n"));
        let calls = &services.runner.state.borrow().calls;
        if startup == Startup::UserSignIn {
            assert!(record.definition.contains(&format!(
                "Restart=no\nRestartForceExitStatus={}\nRestartSec=2s",
                crate::run_exit::LOGIN_PROOF_UNAVAILABLE_EXIT_CODE
            )));
            assert!(!record.definition.contains("Restart=on-failure"));
            assert!(record.definition.contains("StartLimitIntervalSec=0\n"));
            assert!(calls.iter().any(|request| matches!(request,
                NativeRequest::Process { args, .. }
                if args == &["--user", "enable", "--now", &record.resource]
                    .map(std::ffi::OsString::from))));
        } else {
            assert!(record.definition.contains("Restart=no\n"));
            assert!(!record.definition.contains("RestartForceExitStatus="));
            assert!(!record.definition.contains("StartLimitIntervalSec="));
            assert!(!calls.iter().any(|request| matches!(request,
                NativeRequest::Process { args, .. }
                if args.iter().any(|arg| arg == "--now" || arg == "enable" || arg == "start"))));
        }
    }
}

#[cfg(unix)]
#[test]
fn native_service_linux_sign_in_raced_revocation_retains_exact_reservation_without_success() {
    struct Revoking<'a> {
        store: &'a Store,
        inner: FakeRunner,
    }
    impl CommandRunner for Revoking<'_> {
        fn execute(&self, request: &NativeRequest) -> io::Result<NativeOutput> {
            if matches!(request, NativeRequest::Process { args, .. }
                if args.iter().any(|arg| arg == "--now"))
            {
                self.store.revoke()?;
            }
            self.inner.execute(request)
        }
    }
    let root = storage::private_test_directory();
    let store = Store::create(
        &root.path().join("state"),
        Origin::Headless,
        Settings {
            startup: Startup::UserSignIn,
            ..settings()
        },
    )
    .unwrap();
    let before = provision(&store);
    let stage = trusted::fixture(root.path());
    let original = services(root.path(), &store, Platform::Linux);
    let services = Services {
        store: original.store,
        state: original.state,
        context: original.context,
        runner: Revoking {
            store: &store,
            inner: original.runner,
        },
    };
    assert!(services.install(&stage, before.epoch).is_err());
    let current = store.snapshot().unwrap();
    assert_eq!(current.phase, Phase::StopRequested);
    assert_eq!(current.binding, before.binding);
    assert!(current.epoch > before.epoch);
    let retained = services.registration().unwrap().1.unwrap();
    assert_eq!(retained.epoch, before.epoch);
    assert_eq!(retained.binding, before.binding.unwrap());
    assert_eq!(retained.settings.startup, Startup::UserSignIn);
    assert!(services.runner.inner.state.borrow().running);
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
fn native_service_held_start_guard_reopen_reaches_registration_without_reacquiring() {
    let root = storage::private_test_directory();
    let store = Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
    let snapshot = provision(&store);
    let services = services(root.path(), &store, Platform::Linux);
    let guard = services.lease().unwrap().drain_before_drop();
    let error = services
        .reopen_stage_admitted(snapshot.epoch, Phase::Provisioned, &guard)
        .err()
        .unwrap();
    assert_eq!(
        error.to_string(),
        "host.service_not_installed",
        "held guard must reach actual registration boundary rather than nested Busy"
    );
    assert!(services.runner.state.borrow().calls.is_empty());
    drop(guard);
}

#[cfg(unix)]
#[test]
fn native_service_dedicated_start_reuses_existing_expired_guard_without_reset() {
    let root = storage::private_test_directory();
    let store = Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
    let snapshot = provision(&store);
    let stage = trusted::fixture(root.path());
    let services = services(root.path(), &store, Platform::Linux);
    services.install(&stage, snapshot.epoch).unwrap();
    services.runner.state.borrow_mut().calls.clear();
    let guard = services.lease().unwrap().drain_before_drop();
    native::expire_operation_for_test();
    let error = services
        .start_admitted(
            &stage,
            snapshot.epoch,
            native::check_operation_for_test,
            || Ok(()),
            guard,
        )
        .err()
        .unwrap();
    assert_eq!(error.to_string(), "host.service_native_timeout");
    assert!(
        services.runner.state.borrow().calls.is_empty(),
        "expired original operation cannot submit manager work"
    );
}

#[cfg(unix)]
#[test]
fn native_service_parent_revocation_after_credential_read_prevents_unsent_start() {
    let root = storage::private_test_directory();
    let store = Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
    let snapshot = provision(&store);
    let stage = trusted::fixture(root.path());
    let services = services(root.path(), &store, Platform::Linux);
    services.install(&stage, snapshot.epoch).unwrap();
    let revoked = std::cell::Cell::new(false);
    services.runner.state.borrow_mut().calls.clear();
    let error = services
        .start_checked(
            &stage,
            snapshot.epoch,
            || {
                revoked.set(true);
                Ok(())
            },
            || {
                if revoked.get() {
                    Err(refused("host.cloud_cancelled"))
                } else {
                    Ok(())
                }
            },
        )
        .err()
        .unwrap();
    assert_eq!(error.to_string(), "host.cloud_cancelled");
    assert!(
        services.runner.state.borrow().calls.is_empty(),
        "no manager request after revoked read"
    );
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
    if !cfg!(all(feature = "cloud", feature = "host-runtime")) {
        assert!(services.install(&stage, snapshot.epoch).is_err());
        assert_eq!(
            services
                .replace_stopped(&stage, "0.15.0", snapshot.epoch)
                .err()
                .unwrap()
                .to_string(),
            "host.service_windows_authenticated_graceful_stop_unavailable"
        );
        assert!(services
            .start_with(&stage, snapshot.epoch, || panic!(
                "disabled Windows start must not invoke credentials"
            ))
            .is_err());
        assert!(services.runner.state.borrow().calls.is_empty());
    }
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
    #[cfg(not(all(feature = "cloud", feature = "host-runtime")))]
    assert!(services.stop(store.snapshot().unwrap().epoch).is_err());
    #[cfg(all(feature = "cloud", feature = "host-runtime"))]
    {
        services.runner.state.borrow_mut().quiesce_after_query = true;
        assert_eq!(
            services
                .stop(store.snapshot().unwrap().epoch)
                .unwrap()
                .process,
            ProcessState::Stopped
        );
    }
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

#[cfg(all(feature = "cloud", feature = "host-runtime"))]
#[test]
fn native_service_windows_stop_retries_transient_read_contention_without_task_mutation() {
    use std::cell::Cell;
    struct BusyOnce {
        native: FakeRunner,
        busy: Cell<bool>,
    }
    impl CommandRunner for BusyOnce {
        fn execute(&self, request: &NativeRequest) -> io::Result<NativeOutput> {
            if matches!(
                request,
                NativeRequest::Task {
                    action: TaskAction::Query,
                    ..
                }
            ) && self.busy.replace(false)
            {
                self.native.state.borrow_mut().calls.push(request.clone());
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "fixture.read_busy",
                ));
            }
            self.native.execute(request)
        }
    }
    let root = storage::private_test_directory();
    let store = Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
    let before = provision(&store);
    let stage = trusted::fixture(root.path());
    let original = services(root.path(), &store, Platform::Windows);
    original.install(&stage, before.epoch).unwrap();
    original
        .start_with(&stage, before.epoch, || Ok(()))
        .unwrap();
    store.revoke().unwrap();
    let revoked = store.snapshot().unwrap();
    original.runner.state.borrow_mut().calls.clear();
    original.runner.state.borrow_mut().quiesce_after_query = true;
    let services = Services {
        store: &store,
        state: original.state,
        context: original.context,
        runner: BusyOnce {
            native: original.runner,
            busy: Cell::new(true),
        },
    };
    assert_eq!(
        services.stop(revoked.epoch).unwrap().process,
        ProcessState::Stopped
    );
    assert_eq!(store.snapshot().unwrap(), revoked);
    let state = services.runner.native.state.borrow();
    assert!(state.calls.len() >= 2);
    assert!(state.calls.iter().all(|request| matches!(
        request,
        NativeRequest::Task {
            action: TaskAction::Query,
            ..
        }
    )));
}

#[cfg(all(feature = "cloud", feature = "host-runtime"))]
#[test]
fn native_service_windows_stop_contention_keeps_one_deadline_and_revocation() {
    let root = storage::private_test_directory();
    let state = root.path().join("state");
    let store = Store::create(&state, Origin::Headless, settings()).unwrap();
    provision(&store);
    store.revoke().unwrap();
    let revoked = store.snapshot().unwrap();
    let services = fixture_services(&store, &state).unwrap();
    let held = services.lease().unwrap();
    let started = std::time::Instant::now();
    let error = services
        .stop_windows_revocation(revoked.epoch, std::time::Duration::from_millis(40))
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    assert_eq!(error.to_string(), "host.runtime_stop_pending");
    assert!(started.elapsed() >= std::time::Duration::from_millis(40));
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(store.snapshot().unwrap(), revoked);
    drop(held);
    assert_eq!(
        services.stop(revoked.epoch).unwrap().process,
        ProcessState::Absent
    );
    assert_eq!(store.snapshot().unwrap(), revoked);
}

#[cfg(all(feature = "cloud", feature = "host-runtime"))]
#[test]
fn native_service_windows_stop_rechecks_scope_after_constructor_contention() {
    let root = storage::private_test_directory();
    let state = root.path().join("state");
    let store = Store::create(&state, Origin::Headless, settings()).unwrap();
    provision(&store);
    store.revoke().unwrap();
    let revoked = store.snapshot().unwrap();
    let mut held = Some(store.with_owned_lease(|_, _, lease| Ok(lease)).unwrap());
    let mut calls = 0;
    let error = windows_stop_observation(std::time::Duration::from_secs(1), |deadline| {
        calls += 1;
        let result = fixture_services(&store, &state)
            .and_then(|services| services.stop_once(revoked.epoch, deadline));
        if let Some(lease) = held.take() {
            assert_eq!(
                result.as_ref().err().unwrap().kind(),
                io::ErrorKind::WouldBlock
            );
            drop(lease);
            // A different exact foreground completion advances into cleanup.
            store.confirm_stopped(revoked.epoch, revoked.binding.as_ref().unwrap())?;
            store.request_cleanup()?;
        }
        result
    })
    .err()
    .unwrap();
    assert_eq!(calls, 2);
    assert_eq!(
        error.to_string(),
        "host.service_stale_or_unapproved_operation"
    );
    let current = store.snapshot().unwrap();
    assert_eq!(current.phase, Phase::CleanupPending);
    assert!(current.epoch > revoked.epoch);
}

#[cfg(all(feature = "cloud", feature = "host-runtime"))]
#[test]
fn native_service_windows_stop_never_retries_foreign_state_refusal() {
    let root = storage::private_test_directory();
    let other = storage::private_test_directory();
    let state = root.path().join("state");
    let store = Store::create(&state, Origin::Headless, settings()).unwrap();
    provision(&store);
    store.revoke().unwrap();
    let revoked = store.snapshot().unwrap();
    let mut calls = 0;
    let error = windows_stop_observation(std::time::Duration::from_secs(1), |deadline| {
        calls += 1;
        fixture_services(&store, &other.path().join("state"))?.stop_once(revoked.epoch, deadline)
    })
    .err()
    .unwrap();
    assert_eq!(calls, 1);
    assert_ne!(error.kind(), io::ErrorKind::WouldBlock);
    assert_eq!(store.snapshot().unwrap(), revoked);
}

#[cfg(all(feature = "cloud", feature = "host-runtime"))]
#[test]
fn native_service_windows_revocation_reconciles_exact_task_without_destructive_stop() {
    let root = storage::private_test_directory();
    let store = Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
    let before = provision(&store);
    let stage = trusted::fixture(root.path());
    let services = services(root.path(), &store, Platform::Windows);
    services.install(&stage, before.epoch).unwrap();
    assert_eq!(
        services
            .start_with(&stage, before.epoch, || Ok(()))
            .unwrap()
            .process,
        ProcessState::Running
    );
    assert!(services.stop(before.epoch).is_err());
    store.revoke().unwrap();
    let revoked = store.snapshot().unwrap();
    assert!(services.stop(before.epoch).is_err());
    let record = services.registration().unwrap().1.unwrap();
    for transitioning in [false, true] {
        services.runner.state.borrow_mut().pending_start = transitioning;
        let _lease = services.lease().unwrap();
        let error = services
            .wait_windows_revocation(&revoked, &record, std::time::Duration::ZERO)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(store.snapshot().unwrap(), revoked);
    }
    services.runner.state.borrow_mut().pending_start = false;
    services.runner.state.borrow_mut().quiesce_after_query = true;
    assert_eq!(
        services.stop(revoked.epoch).unwrap().process,
        ProcessState::Stopped
    );
    services.runner.state.borrow_mut().failed = true;
    assert_eq!(
        services.stop(revoked.epoch).unwrap().process,
        ProcessState::Failed
    );
    services.runner.state.borrow_mut().failed = false;
    services.runner.state.borrow_mut().observation_error = true;
    assert!(services.stop(revoked.epoch).is_err());
    services.runner.state.borrow_mut().observation_error = false;
    services.runner.state.borrow_mut().native_security = Some("foreign".into());
    assert!(services.stop(revoked.epoch).is_err());
    services.runner.state.borrow_mut().native_security = None;
    services.runner.state.borrow_mut().native_xml = Some("foreign".into());
    assert!(services.stop(revoked.epoch).is_err());
    services.runner.state.borrow_mut().native_xml = None;
    assert_eq!(
        services.stop(revoked.epoch).unwrap().process,
        ProcessState::Absent
    );
    assert!(!services
        .runner
        .state
        .borrow()
        .calls
        .iter()
        .any(|request| matches!(
            request,
            NativeRequest::Process { .. }
                | NativeRequest::Task {
                    action: TaskAction::Stop,
                    ..
                }
        )));
}

#[cfg(all(feature = "cloud", feature = "host-runtime", unix))]
#[test]
fn native_service_start_zero_admin_requires_fresh_provider_snapshot_and_keeps_owner_only_strict() {
    for mode in [
        crate::lifecycle::OrdinaryAccess::OwnerOnly,
        crate::lifecycle::OrdinaryAccess::ProviderOwner,
    ] {
        let (root, store, vault) = crate::enrollment::tests::setup_origin(Origin::Desktop);
        let owners = crate::enrollment::tests::fixture_owners(&store, vault.clone());
        owners
            .initialize(Path::new("host-identity"), "github:1234".into())
            .unwrap();
        if mode == crate::lifecycle::OrdinaryAccess::ProviderOwner {
            reconfigure_lifecycle_mode(&store, false, mode);
        }
        let owner = owners.snapshot().unwrap();
        assert!(owner.keys.is_empty() && !owner.remote_access);
        let snapshot = store.snapshot().unwrap();
        let mut services = services(root.path(), &store, Platform::Linux);
        services.state = root.path().join("installation");
        services.runner.receipt = services.state.join(RECORD);
        let stage = trusted::fixture(root.path());
        services
            .install_with_recovery(&stage, snapshot.epoch)
            .unwrap();
        let before = services.runner.state.borrow().calls.len();
        let result = services.start_with(&stage, snapshot.epoch, || {
            services.require_start_credentials_with(&owners)
        });
        if mode == crate::lifecycle::OrdinaryAccess::ProviderOwner {
            assert_eq!(result.unwrap().process, ProcessState::Running);
            assert!(services.runner.state.borrow().running);
            vault
                .locked
                .store(true, std::sync::atomic::Ordering::Release);
            let calls = services.runner.state.borrow().calls.len();
            assert!(services
                .start_with(&stage, snapshot.epoch, || services
                    .require_start_credentials_with(&owners))
                .is_err());
            assert_eq!(services.runner.state.borrow().calls.len(), calls);
            vault
                .locked
                .store(false, std::sync::atomic::Ordering::Release);
        } else {
            assert!(result.is_err());
            assert!(!services.runner.state.borrow().running);
            assert_eq!(services.runner.state.borrow().calls.len(), before);
        }
        assert!(owners.snapshot().unwrap().keys.is_empty());
    }
}

// Model the observed Scheduler-only XML insertion. The native boundary's COM
// serialization and descriptor policy are qualified separately on Windows.
fn pending_windows_descriptor(
    services: &Services<'_, FakeRunner>,
    epoch: u64,
) -> (Registration, Registration) {
    let mut before = services.registration().unwrap().1.unwrap();
    before.native_snapshot = None;
    let legacy = before.native_expected.clone().unwrap();
    let raw = legacy.replace(
        "<RegistrationInfo>",
        "<RegistrationInfo><SecurityDescriptor>checked-owner-system-policy</SecurityDescriptor>",
    );
    assert_ne!(legacy, raw);
    services.save(&before, epoch, Phase::Provisioned).unwrap();
    let mut scheduler = services.runner.state.borrow_mut();
    scheduler.native_xml = Some(raw.clone());
    scheduler.native_prepared_upgrade = Some((legacy, raw.clone()));
    scheduler.calls.clear();
    let mut after = before.clone();
    after.native_expected = Some(raw.clone());
    after.native_snapshot = Some(raw);
    (before, after)
}

#[test]
fn native_service_windows_pending_descriptor_promotes_raw_v4_snapshot_without_recreate() {
    let root = storage::private_test_directory();
    let store = Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
    let snapshot = provision(&store);
    let stage = trusted::fixture(root.path());
    let services = services(root.path(), &store, Platform::Windows);
    services
        .install_reserved_policy(&stage, snapshot.epoch, true)
        .unwrap();
    let (before, after) = pending_windows_descriptor(&services, snapshot.epoch);
    assert_eq!(before.version, 4);
    assert_eq!(services.status().unwrap().process, ProcessState::Stopped);
    let remembered = services.registration().unwrap().1.unwrap();
    assert_eq!(
        remembered, after,
        "only expected and raw snapshot may change"
    );
    // Preserve the pre-change helper's v4 raw equality/readback contract.
    let old_format: Registration =
        serde_json::from_slice(&serde_json::to_vec(&remembered).unwrap()).unwrap();
    services
        .validate_registration(&old_format, &snapshot)
        .unwrap();
    assert_eq!(old_format.native_snapshot, old_format.native_expected);
    assert_eq!(
        old_format.native_snapshot,
        services.runner.state.borrow().native_xml
    );
    let calls = &services.runner.state.borrow().calls;
    assert_eq!(
        calls
            .iter()
            .filter(|c| matches!(
                c,
                NativeRequest::Task {
                    action: TaskAction::Prepare(_),
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(calls.iter().all(|c| matches!(
        c,
        NativeRequest::Task {
            action: TaskAction::Query | TaskAction::Prepare(_),
            ..
        }
    )));
}

#[test]
fn native_service_windows_existing_raw_snapshot_is_never_reprepared_or_rewritten() {
    let root = storage::private_test_directory();
    let store = Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
    let snapshot = provision(&store);
    let stage = trusted::fixture(root.path());
    let services = services(root.path(), &store, Platform::Windows);
    services
        .install_reserved_policy(&stage, snapshot.epoch, true)
        .unwrap();
    let (_, raw) = pending_windows_descriptor(&services, snapshot.epoch);
    services
        .save(&raw, snapshot.epoch, Phase::Provisioned)
        .unwrap();
    let bytes = std::fs::read(&services.runner.receipt).unwrap();
    services.runner.state.borrow_mut().native_prepared_upgrade = Some((
        raw.native_expected.clone().unwrap(),
        "must-not-reprepare".into(),
    ));
    for _ in 0..2 {
        assert_eq!(services.status().unwrap().process, ProcessState::Stopped);
        assert_eq!(std::fs::read(&services.runner.receipt).unwrap(), bytes);
    }
    assert!(services
        .runner
        .state
        .borrow()
        .calls
        .iter()
        .all(|c| matches!(
            c,
            NativeRequest::Task {
                action: TaskAction::Query,
                ..
            }
        )));
}

#[test]
fn native_service_windows_descriptor_recovery_refuses_changed_authority_and_unsent_intent() {
    for mutation in 0..10 {
        let root = storage::private_test_directory();
        let store =
            Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
        let snapshot = provision(&store);
        let stage = trusted::fixture(root.path());
        let services = services(root.path(), &store, Platform::Windows);
        services
            .install_reserved_policy(&stage, snapshot.epoch, true)
            .unwrap();
        let (mut before, _) = pending_windows_descriptor(&services, snapshot.epoch);
        if mutation == 0 {
            before.native_submitted = false;
            services
                .save(&before, snapshot.epoch, Phase::Provisioned)
                .unwrap();
        } else {
            let mut scheduler = services.runner.state.borrow_mut();
            if mutation == 1 {
                scheduler.native_security = Some("foreign-policy".into());
            } else if mutation == 2 {
                scheduler.native_xml = None;
            } else {
                let (needle, replacement) = match mutation {
                    3 => ("<Command>", "<Command>foreign-executable-"),
                    4 => ("<Arguments>", "<Arguments>foreign-argument "),
                    5 => ("<UserId>", "<UserId>foreign-principal-"),
                    6 => ("LeastPrivilege", "HighestAvailable"),
                    7 => ("<URI>", "<URI>foreign-resource-"),
                    8 => ("fesTerm-host-reservation:", "foreign-reservation:"),
                    _ => ("IgnoreNew", "Parallel"),
                };
                let raw = scheduler.native_xml.as_mut().unwrap();
                assert!(raw.contains(needle));
                *raw = raw.replace(needle, replacement);
            }
        }
        let bytes = std::fs::read(&services.runner.receipt).unwrap();
        assert!(services.status().is_err(), "mutation {mutation}");
        assert_eq!(std::fs::read(&services.runner.receipt).unwrap(), bytes);
        assert!(services
            .runner
            .state
            .borrow()
            .calls
            .iter()
            .all(|c| matches!(
                c,
                NativeRequest::Task {
                    action: TaskAction::Query | TaskAction::Prepare(_),
                    ..
                }
            )));
    }
}

#[test]
fn native_service_windows_descriptor_promotion_cas_preserves_concurrent_receipt() {
    for mutation in 0..3 {
        let root = storage::private_test_directory();
        let store =
            Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
        let snapshot = provision(&store);
        let stage = trusted::fixture(root.path());
        let services = services(root.path(), &store, Platform::Windows);
        services
            .install_reserved_policy(&stage, snapshot.epoch, true)
            .unwrap();
        let (before, _) = pending_windows_descriptor(&services, snapshot.epoch);
        let mut concurrent = before.clone();
        match mutation {
            0 => concurrent.native_submitted = false,
            1 => concurrent.native_expected.as_mut().unwrap().push(' '),
            _ => concurrent.native_snapshot = concurrent.native_expected.clone(),
        }
        services
            .runner
            .state
            .borrow_mut()
            .replace_receipt_on_prepare = Some(concurrent.clone());
        assert!(services.status().is_err());
        assert_eq!(services.registration().unwrap().1.unwrap(), concurrent);
        assert!(services
            .runner
            .state
            .borrow()
            .calls
            .iter()
            .all(|c| matches!(
                c,
                NativeRequest::Task {
                    action: TaskAction::Query | TaskAction::Prepare(_),
                    ..
                }
            )));
    }
}

#[test]
fn native_service_windows_fresh_reservation_retains_raw_snapshot_and_never_starts() {
    let root = storage::private_test_directory();
    let store = Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
    let snapshot = provision(&store);
    let stage = trusted::fixture(root.path());
    let services = services(root.path(), &store, Platform::Windows);
    services
        .install_reserved_policy(&stage, snapshot.epoch, true)
        .unwrap();
    let record = services.registration().unwrap().1.unwrap();
    assert_eq!(record.version, 4);
    assert_eq!(record.native_expected, record.native_snapshot);
    let scheduler = services.runner.state.borrow();
    assert_eq!(record.native_snapshot, scheduler.native_xml);
    assert_eq!(
        scheduler
            .calls
            .iter()
            .filter(|c| matches!(
                c,
                NativeRequest::Task {
                    action: TaskAction::Create(_),
                    ..
                }
            ))
            .count(),
        1
    );
    assert_eq!(
        scheduler
            .calls
            .iter()
            .filter(|c| matches!(
                c,
                NativeRequest::Task {
                    action: TaskAction::Prepare(_),
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(!scheduler.calls.iter().any(|c| matches!(
        c,
        NativeRequest::Task {
            action: TaskAction::Start,
            ..
        }
    )));
}

#[cfg(all(feature = "cloud", feature = "host-runtime"))]
#[path = "windows_worker_tests.rs"]
mod windows_worker_tests;

#[cfg(unix)]
#[test]
fn native_service_start_credential_delay_exhausts_inherited_budget_without_effects() {
    struct DeadlineRunner(FakeRunner);
    impl CommandRunner for DeadlineRunner {
        fn execute(&self, request: &NativeRequest) -> io::Result<NativeOutput> {
            if native::check_operation_for_test().is_err() {
                // Exercise the real runner's pre-spawn refusal after expiry;
                // still-live requests use the owned in-process manager fixture.
                return native::NativeRunner.execute(request);
            }
            self.0.execute(request)
        }
    }
    for expire_at in [1, 2] {
        let root = storage::private_test_directory();
        let store =
            Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
        let snapshot = provision(&store);
        let stage = trusted::fixture(root.path());
        let base = services(root.path(), &store, Platform::Linux);
        base.install(&stage, snapshot.epoch).unwrap();
        base.runner.state.borrow_mut().calls.clear();
        let active = Services {
            store: &store,
            state: base.state,
            context: base.context,
            runner: DeadlineRunner(base.runner),
        };
        let checks = std::cell::Cell::new(0);
        let error = active
            .start_with(&stage, snapshot.epoch, || {
                checks.set(checks.get() + 1);
                if checks.get() == expire_at {
                    native::expire_operation_for_test();
                }
                Ok(())
            })
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "host.service_native_timeout");
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(checks.get(), expire_at);
        let state = active.runner.0.state.borrow();
        assert!(!state.running);
        assert!(state.calls.iter().all(|request| matches!(request,
            NativeRequest::Process { args, .. } if args.iter().any(|arg| arg == "show"))));
        assert_eq!(state.calls.len(), usize::from(expire_at == 2));
        drop(state);
        assert_eq!(store.snapshot().unwrap(), snapshot);
        assert!(!active.state.join("host-runtime.json").exists());
    }
}

#[cfg(all(feature = "cloud", feature = "host-runtime", unix))]
#[test]
fn native_service_start_credential_observations_use_ni_reads_and_keep_locked_refusal() {
    for locked in [false, true] {
        let (root, store, vault) = crate::enrollment::tests::setup_origin(Origin::Desktop);
        let owners = crate::enrollment::tests::fixture_owners(&store, vault.clone());
        owners
            .initialize(Path::new("host-identity"), "github:1234".into())
            .unwrap();
        reconfigure_lifecycle_mode(
            &store,
            false,
            crate::lifecycle::OrdinaryAccess::ProviderOwner,
        );
        owners.snapshot().unwrap();
        let snapshot = store.snapshot().unwrap();
        let mut active = services(root.path(), &store, Platform::Linux);
        active.state = root.path().join("installation");
        active.runner.receipt = active.state.join(RECORD);
        let stage = trusted::fixture(root.path());
        active
            .install_with_recovery(&stage, snapshot.epoch)
            .unwrap();
        let interactive = vault
            .interactive_reads
            .load(std::sync::atomic::Ordering::Acquire);
        let ni = vault
            .noninteractive_reads
            .load(std::sync::atomic::Ordering::Acquire);
        vault
            .locked
            .store(locked, std::sync::atomic::Ordering::Release);
        let repeated = std::cell::Cell::new(false);
        let result = active.start_with(&stage, snapshot.epoch, || {
            active.require_start_credentials_observed_with(
                snapshot.epoch,
                repeated.replace(true),
                || {
                    Ok(crate::enrollment::tests::fixture_owners(
                        &store,
                        vault.clone(),
                    ))
                },
            )
        });
        if locked {
            assert_eq!(
                result.err().unwrap().to_string(),
                "owner.vault_locked_or_unavailable_unlock_native_store_and_retry"
            );
            assert!(!active.runner.state.borrow().running);
        } else {
            assert_eq!(result.unwrap().process, ProcessState::Running);
        }
        assert_eq!(
            vault
                .interactive_reads
                .load(std::sync::atomic::Ordering::Acquire),
            interactive
        );
        assert_eq!(
            vault
                .noninteractive_reads
                .load(std::sync::atomic::Ordering::Acquire)
                - ni,
            if locked { 1 } else { 2 }
        );
        let log = std::fs::read_to_string(active.state.join("worker.log")).unwrap();
        assert!(log.contains("host.owner_start_noninteractive"));
        assert!(log.contains("check=initial stage=anchor_read"));
        assert_eq!(
            log.contains("check=before_mutation stage=anchor_read"),
            !locked
        );
        assert_eq!(
            log.contains("stage=anchor_read outcome=error category=locked_or_unavailable"),
            locked
        );
        assert!(!log.contains("github:1234"));
        assert!(!log.contains("run="));
        assert!(!log.contains(&active.state.display().to_string()));
    }
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum FailedBootout {
    Absent,
    Running,
    Stopped,
    ForeignDefinition,
    MissingDefinition,
    ChangedReceipt,
    ChangedEpoch,
    MalformedObservation,
    ObservationError,
    ChangedEpochAfterObservation,
    ForeignDefinitionAfterObservation,
    SignalExit,
    InconsistentExit,
    ExecutionError,
}

#[cfg(unix)]
struct FailedBootoutRunner<'a> {
    native: FakeRunner,
    store: &'a Store,
    mode: FailedBootout,
    attempted: std::cell::Cell<bool>,
}

#[cfg(unix)]
impl CommandRunner for FailedBootoutRunner<'_> {
    fn execute(&self, request: &NativeRequest) -> io::Result<NativeOutput> {
        let mut output = self.native.execute(request)?;
        let NativeRequest::Process { args, .. } = request else {
            return Ok(output);
        };
        if args.iter().any(|arg| arg == "bootout") {
            self.attempted.set(true);
            match self.mode {
                FailedBootout::Running | FailedBootout::Stopped => {
                    let mut state = self.native.state.borrow_mut();
                    state.registered = true;
                    state.running = matches!(self.mode, FailedBootout::Running);
                }
                FailedBootout::ForeignDefinition => {
                    std::fs::write(&self.native.definition, b"foreign")?;
                }
                FailedBootout::MissingDefinition => {
                    std::fs::remove_file(&self.native.definition)?;
                }
                FailedBootout::ChangedReceipt => {
                    let mut record: Registration =
                        serde_json::from_slice(&std::fs::read(&self.native.receipt)?)?;
                    record.removed = true;
                    let directory =
                        storage::private_directory(self.native.receipt.parent().unwrap(), false)?;
                    storage::replace_private(
                        &directory,
                        Path::new(RECORD),
                        &serde_json::to_vec(&record)?,
                    )?;
                }
                FailedBootout::ChangedEpoch => self.store.revoke()?,
                FailedBootout::ExecutionError => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "fixture.bootout_execution_failure",
                    ));
                }
                _ => {}
            }
            output.success = false;
            output.code = match self.mode {
                FailedBootout::SignalExit => -1,
                FailedBootout::InconsistentExit => 0,
                _ => 3,
            };
        } else if self.attempted.get() && args.first().is_some_and(|arg| arg == "print") {
            match self.mode {
                FailedBootout::ChangedEpochAfterObservation => self.store.revoke()?,
                FailedBootout::ForeignDefinitionAfterObservation => {
                    std::fs::write(&self.native.definition, b"foreign")?;
                }
                FailedBootout::MalformedObservation => {
                    output.success = true;
                    output.code = 0;
                    output.stdout = b"unqualified manager output".to_vec();
                }
                FailedBootout::ObservationError => {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "fixture.observe",
                    ));
                }
                _ => {}
            }
        }
        Ok(output)
    }
}

#[cfg(unix)]
fn failed_bootout_fixture<'a>(
    store: &'a Store,
    base: Services<'a, FakeRunner>,
    mode: FailedBootout,
) -> Services<'a, FailedBootoutRunner<'a>> {
    Services {
        store,
        state: base.state,
        context: base.context,
        runner: FailedBootoutRunner {
            native: base.runner,
            store,
            mode,
            attempted: std::cell::Cell::new(false),
        },
    }
}

#[cfg(unix)]
#[test]
fn native_service_macos_failed_bootout_finishes_only_after_fresh_exact_absence() {
    let root = storage::private_test_directory();
    let store = Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
    let snapshot = provision(&store);
    let stage = trusted::fixture(root.path());
    let base = services(root.path(), &store, Platform::Macos);
    base.install(&stage, snapshot.epoch).unwrap();
    base.start_with(&stage, snapshot.epoch, || Ok(())).unwrap();
    store.revoke().unwrap();
    let revoked = store.snapshot().unwrap();
    let receipt = std::fs::read(&base.runner.receipt).unwrap();
    let manager = failed_bootout_fixture(&store, base, FailedBootout::Absent);
    let observed = manager.stop(revoked.epoch).unwrap();
    assert_eq!(observed.process, ProcessState::Absent);
    assert_eq!(store.snapshot().unwrap(), revoked);
    assert_eq!(
        std::fs::read(&manager.runner.native.receipt).unwrap(),
        receipt
    );
    assert_eq!(manager.runner.native.state.borrow().calls.iter().filter(|request|
        matches!(request, NativeRequest::Process { args, .. } if args.iter().any(|arg| arg == "bootout"))
    ).count(), 1);
    store
        .confirm_stopped(revoked.epoch, revoked.binding.as_ref().unwrap())
        .unwrap();
    assert_eq!(store.snapshot().unwrap().phase, Phase::Disabled);
}

#[cfg(unix)]
#[test]
fn native_service_macos_failed_bootout_preserves_error_without_exact_absence_proof() {
    for mode in [
        FailedBootout::Running,
        FailedBootout::Stopped,
        FailedBootout::ForeignDefinition,
        FailedBootout::MissingDefinition,
        FailedBootout::ChangedReceipt,
        FailedBootout::ChangedEpoch,
        FailedBootout::MalformedObservation,
        FailedBootout::ObservationError,
        FailedBootout::ChangedEpochAfterObservation,
        FailedBootout::ForeignDefinitionAfterObservation,
        FailedBootout::SignalExit,
        FailedBootout::InconsistentExit,
        FailedBootout::ExecutionError,
    ] {
        let root = storage::private_test_directory();
        let store =
            Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
        let snapshot = provision(&store);
        let stage = trusted::fixture(root.path());
        let base = services(root.path(), &store, Platform::Macos);
        base.install(&stage, snapshot.epoch).unwrap();
        base.start_with(&stage, snapshot.epoch, || Ok(())).unwrap();
        store.revoke().unwrap();
        let revoked = store.snapshot().unwrap();
        let manager = failed_bootout_fixture(&store, base, mode);
        let error = manager.stop(revoked.epoch).err().unwrap();
        if matches!(mode, FailedBootout::ExecutionError) {
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert_eq!(error.to_string(), "fixture.bootout_execution_failure");
        } else {
            assert_eq!(error.to_string(), "host.service_native_operation_failed");
        }
        assert_eq!(store.snapshot().unwrap().phase, Phase::StopRequested);
        assert_eq!(manager.runner.native.state.borrow().calls.iter().filter(|request|
            matches!(request, NativeRequest::Process { args, .. } if args.iter().any(|arg| arg == "bootout"))
        ).count(), 1);
    }
}

thread_local! {
    static MACOS_STOP_BUSY: RefCell<Option<std::sync::mpsc::SyncSender<()>>> = const { RefCell::new(None) };
}

pub(super) fn macos_stop_busy_observed() {
    MACOS_STOP_BUSY.with(|slot| {
        if let Some(signal) = slot.borrow_mut().take() {
            let _ = signal.send(());
        }
    });
}

#[cfg(unix)]
#[test]
fn native_service_macos_completed_bootout_waits_for_same_epoch_read_lease() {
    struct ContendedBootout {
        native: FakeRunner,
        release: RefCell<Option<std::thread::JoinHandle<bool>>>,
        held: RefCell<Option<Lease>>,
        expired: bool,
    }
    impl CommandRunner for ContendedBootout {
        fn execute(&self, request: &NativeRequest) -> io::Result<NativeOutput> {
            let output = self.native.execute(request)?;
            if matches!(request, NativeRequest::Process { args, .. } if args.iter().any(|arg| arg == "bootout"))
            {
                let directory =
                    storage::private_directory(self.native.receipt.parent().unwrap(), false)?;
                let lease = Lease::exclusive(storage::open_private_file(
                    &directory,
                    Path::new("host-lifecycle.lock"),
                    false,
                )?)?;
                if self.expired {
                    *self.held.borrow_mut() = Some(lease);
                } else {
                    let (send, receive) = std::sync::mpsc::sync_channel(1);
                    MACOS_STOP_BUSY.with(|slot| *slot.borrow_mut() = Some(send));
                    *self.release.borrow_mut() = Some(std::thread::spawn(move || {
                        // Hold until the actual post-stop read returns Busy.
                        // A missing notification has bounded cleanup and fails
                        // the assertion, never silently becoming a timed release.
                        let observed = receive
                            .recv_timeout(std::time::Duration::from_secs(5))
                            .is_ok();
                        drop(lease);
                        observed
                    }));
                }
            }
            Ok(output)
        }
    }
    for expired in [false, true] {
        let root = storage::private_test_directory();
        let store =
            Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
        let snapshot = provision(&store);
        let stage = trusted::fixture(root.path());
        let base = services(root.path(), &store, Platform::Macos);
        base.install(&stage, snapshot.epoch).unwrap();
        base.start_with(&stage, snapshot.epoch, || Ok(())).unwrap();
        store.revoke().unwrap();
        let revoked = store.snapshot().unwrap();
        let receipt = std::fs::read(&base.runner.receipt).unwrap();
        let manager = Services {
            store: &store,
            state: base.state,
            context: base.context,
            runner: ContendedBootout {
                native: base.runner,
                release: RefCell::new(None),
                held: RefCell::new(None),
                expired,
            },
        };
        let result = if expired {
            manager.stop_once(revoked.epoch, std::time::Instant::now())
        } else {
            manager.stop(revoked.epoch)
        };
        MACOS_STOP_BUSY.with(|slot| drop(slot.borrow_mut().take()));
        if let Some(release) = manager.runner.release.borrow_mut().take() {
            assert!(
                release.join().unwrap(),
                "actual WouldBlock branch must release the holder"
            );
        }
        drop(manager.runner.held.borrow_mut().take());
        if expired {
            let error = result.err().unwrap();
            assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
            assert_eq!(error.to_string(), "host.installation_busy");
        } else {
            assert_eq!(result.unwrap().process, ProcessState::Absent);
        }
        assert_eq!(store.snapshot().unwrap(), revoked);
        assert_eq!(
            std::fs::read(&manager.runner.native.receipt).unwrap(),
            receipt
        );
        assert_eq!(manager.runner.native.state.borrow().calls.iter().filter(|request|
        matches!(request, NativeRequest::Process { args, .. } if args.iter().any(|arg| arg == "bootout"))
    ).count(), 1);
    }
}

#[cfg(unix)]
#[test]
fn native_service_macos_completed_bootout_refuses_changed_post_stop_proof() {
    struct CompletedBootout<'a>(FailedBootoutRunner<'a>);
    impl CommandRunner for CompletedBootout<'_> {
        fn execute(&self, request: &NativeRequest) -> io::Result<NativeOutput> {
            let mut output = self.0.execute(request)?;
            if matches!(request, NativeRequest::Process { args, .. } if args.iter().any(|arg| arg == "bootout"))
            {
                output.success = true;
                output.code = 0;
            }
            Ok(output)
        }
    }
    for mode in [
        FailedBootout::Running,
        FailedBootout::ForeignDefinition,
        FailedBootout::MissingDefinition,
        FailedBootout::ChangedReceipt,
        FailedBootout::ChangedEpoch,
        FailedBootout::ObservationError,
        FailedBootout::ChangedEpochAfterObservation,
        FailedBootout::ForeignDefinitionAfterObservation,
    ] {
        let root = storage::private_test_directory();
        let store =
            Store::create(&root.path().join("state"), Origin::Headless, settings()).unwrap();
        let snapshot = provision(&store);
        let stage = trusted::fixture(root.path());
        let base = services(root.path(), &store, Platform::Macos);
        base.install(&stage, snapshot.epoch).unwrap();
        base.start_with(&stage, snapshot.epoch, || Ok(())).unwrap();
        store.revoke().unwrap();
        let revoked = store.snapshot().unwrap();
        let manager = failed_bootout_fixture(&store, base, mode);
        let manager = Services {
            store: manager.store,
            state: manager.state,
            context: manager.context,
            runner: CompletedBootout(manager.runner),
        };
        assert!(manager.stop(revoked.epoch).is_err());
        assert_eq!(store.snapshot().unwrap().phase, Phase::StopRequested);
        assert_eq!(manager.runner.0.native.state.borrow().calls.iter().filter(|request|
            matches!(request, NativeRequest::Process { args, .. } if args.iter().any(|arg| arg == "bootout"))
        ).count(), 1);
    }
}
