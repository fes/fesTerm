use super::*;
use crate::enrollment::{owner_key_fingerprint, OwnerStorage};
use festerm_tunnel_bridge::{host_identity::HostIdentity, owner_auth::OwnerSigningKey};
use std::{cell::RefCell, collections::BTreeSet, sync::Arc};

struct Model {
    directory: PathBuf,
    loaded: RefCell<BTreeSet<String>>,
    calls: RefCell<Vec<Vec<String>>>,
    reloads: RefCell<usize>,
    fail_reload: RefCell<Option<usize>>,
    revoke_reload: RefCell<Option<(usize, Arc<Store>)>>,
    tamper_reload: RefCell<Option<(usize, PathBuf)>>,
}

impl CommandRunner for Model {
    fn execute(&self, request: &NativeRequest) -> io::Result<NativeOutput> {
        let NativeRequest::Process { args, .. } = request else {
            return Err(refused("fixture.unexpected_task"));
        };
        let args: Vec<String> = args
            .iter()
            .map(|arg| arg.to_str().unwrap().into())
            .collect();
        self.calls.borrow_mut().push(args.clone());
        let mut output = NativeOutput {
            success: true,
            code: 0,
            stdout: Vec::new(),
        };
        if args.iter().any(|arg| arg == "daemon-reload") {
            *self.reloads.borrow_mut() += 1;
            *self.loaded.borrow_mut() = std::fs::read_dir(&self.directory)?
                .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_owned())
                .collect();
            let count = *self.reloads.borrow();
            if self
                .revoke_reload
                .borrow()
                .as_ref()
                .is_some_and(|(at, _)| *at == count)
            {
                self.revoke_reload.borrow_mut().take().unwrap().1.revoke()?;
            }
            if self
                .tamper_reload
                .borrow()
                .as_ref()
                .is_some_and(|(at, _)| *at == count)
            {
                let path = self.tamper_reload.borrow_mut().take().unwrap().1;
                std::fs::write(path, b"changed helper")?;
            }
            if *self.fail_reload.borrow() == Some(count) {
                *self.fail_reload.borrow_mut() = None;
                output.success = false;
                output.code = 1;
            }
        }
        if args.iter().any(|arg| arg == "show") {
            let resource = args
                .iter()
                .find(|arg| arg.starts_with("fesTerm-Host-"))
                .unwrap();
            let filename = format!("{resource}.service");
            let loaded = self.loaded.borrow().contains(&filename);
            let fragment = if loaded {
                self.directory.join(&filename)
            } else {
                PathBuf::new()
            };
            output.stdout = format!(
                "Id={filename}\nLoadState={}\nFragmentPath={}\nDropInPaths=\nActiveState=inactive\nSubState=dead\nMainPID=0\nExecMainPID=0\nResult=success\nExecMainCode=0\nExecMainStatus=0\n",
                if loaded { "loaded" } else { "not-found" }, fragment.display(),
            ).into_bytes();
            if !loaded {
                output.success = false;
                output.code = 1;
            }
        }
        Ok(output)
    }
}

struct Fixture {
    root: storage::PrivateTestDirectory,
    store: Arc<Store>,
    old: TrustedStage,
    next: TrustedStage,
}

impl Fixture {
    fn new() -> Self {
        Self::with_root_and_sessiond(storage::private_test_directory(), None)
    }

    fn with_root_and_sessiond(
        root: storage::PrivateTestDirectory,
        sessiond: Option<&[u8]>,
    ) -> Self {
        let state = root.path().join("state");
        let store = Arc::new(
            Store::create(
                &state,
                Origin::Headless,
                Settings {
                    friendly_name: "Update fixture".into(),
                    allow_creation: false,
                    startup: Startup::Manual,
                },
            )
            .unwrap(),
        );
        let directory = storage::private_directory(&state, false).unwrap();
        let identity = HostIdentity::load_or_create_from_private_directory(
            &directory,
            Path::new("host-identity"),
        )
        .unwrap();
        let ticket = store
            .request_enable(&store.snapshot().unwrap().settings)
            .unwrap();
        store
            .record_provisioned(
                &ticket,
                Binding {
                    host_id: identity.host_id(),
                    account: "github:42".into(),
                    qualified_tunnel: "fixture-qualified".into(),
                    cloud_incarnation: "fixture-incarnation".into(),
                    host_key_fingerprint: identity.fingerprint().unwrap(),
                },
            )
            .unwrap();
        let owners = store
            .owner_enrollment_with_storage(OwnerStorage::HeadlessProtectedFile)
            .unwrap();
        let initial = owners
            .initialize(Path::new("host-identity"), "github:42".into())
            .unwrap();
        let key = OwnerSigningKey::generate().unwrap().public_key();
        let enrolled = owners
            .enroll(initial.revision, key, &owner_key_fingerprint(&key).unwrap())
            .unwrap();
        let permissions = festerm_host_protocol::Capabilities {
            inventory: true,
            ensure: true,
            attach: true,
            detach: true,
            ..Default::default()
        };
        owners
            .approve_permissions(enrolled.revision, true, permissions)
            .unwrap();
        let old = match sessiond {
            Some(bytes) => trusted::version_fixture_with_sessiond(root.path(), "0.15.0", bytes),
            None => trusted::version_fixture(root.path(), "0.15.0"),
        };
        let next = trusted::version_fixture(root.path(), crate::VERSION);
        Self {
            root,
            store,
            old,
            next,
        }
    }

    fn services(&self) -> Services<'_, Model> {
        Services {
            store: &self.store,
            state: self.root.path().join("state"),
            context: Context {
                platform: Platform::Linux,
                user: "1001".into(),
                registration: self.root.path().join("registration"),
            },
            runner: Model {
                directory: self.root.path().join("registration"),
                loaded: RefCell::new(BTreeSet::new()),
                calls: RefCell::new(Vec::new()),
                reloads: RefCell::new(0),
                fail_reload: RefCell::new(None),
                revoke_reload: RefCell::new(None),
                tamper_reload: RefCell::new(None),
            },
        }
    }

    fn installed(&self, services: &Services<'_, Model>) -> Snapshot {
        let before = self.store.snapshot().unwrap();
        services.install(&self.old, before.epoch).unwrap();
        self.store.revoke().unwrap();
        let stopped = self.store.snapshot().unwrap();
        services.stop(stopped.epoch).unwrap();
        self.store
            .confirm_stopped(stopped.epoch, stopped.binding.as_ref().unwrap())
            .unwrap();
        self.store.snapshot().unwrap()
    }

    fn update(&self, services: &Services<'_, Model>) -> io::Result<ReplacementReport> {
        services.replace_stopped_with(
            &self.next,
            "0.15.0",
            self.store.snapshot()?.epoch,
            TrustedStage::authenticate_fixture_registered,
        )
    }

    fn preserved(&self) -> Vec<(String, Vec<u8>)> {
        let mut bytes: Vec<_> = std::fs::read_dir(self.root.path().join("state"))
            .unwrap()
            .filter_map(|entry| {
                let path = entry.unwrap().path();
                let name = path.file_name().unwrap().to_str().unwrap().to_owned();
                if name != "host-lifecycle.json"
                    && name != "host-lifecycle.lock"
                    && !name.starts_with("native-service")
                    && !name.starts_with("host-runtime")
                {
                    if path.is_file() {
                        Some((name, std::fs::read(path).unwrap()))
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();
        bytes.sort();
        bytes
    }
}

#[test]
fn host_update_authenticated_stopped_transaction_preserves_identity_binding_grants_and_complete_helpers(
) {
    let fixture = Fixture::new();
    let services = fixture.services();
    let stopped = fixture.installed(&services);
    let preserved = fixture.preserved();
    assert!(!preserved.is_empty());
    let old = services.registration().unwrap().1.unwrap();
    let report = fixture.update(&services).unwrap();
    assert_eq!(report.state, ReplacementState::Committed);
    assert_eq!(fixture.store.snapshot().unwrap(), stopped);
    assert_eq!(fixture.preserved(), preserved);
    assert!(fixture.old.sessiond_path().unwrap().exists());
    let new = services.registration().unwrap().1.unwrap();
    assert_ne!(old.resource, new.resource);
    assert_ne!(old.executable, new.executable);
    assert_ne!(old.digest, new.digest);
    assert_eq!(new.executable, fixture.next.path());
    assert_eq!(
        new.provenance.as_deref(),
        Some(fixture.next.provenance_path())
    );
    assert!(!definitions::definition_path(&services.context, &old).exists());
    assert!(definitions::definition_path(&services.context, &new).exists());
    let calls = services.runner.calls.borrow().len();
    let again = fixture.update(&services).unwrap();
    assert_eq!(again.operation, report.operation);
    assert!(services.runner.calls.borrow()[calls..]
        .iter()
        .all(|args| args.iter().any(|arg| arg == "show")));
    assert!(services.runner.calls.borrow().iter().all(|args| !args
        .iter()
        .any(|arg| matches!(arg.as_str(), "start" | "kill" | "bootstrap" | "kickstart"))));
}

#[test]
fn host_update_rejects_live_phase_runtime_lease_downgrade_and_changed_helper_before_native_effects()
{
    let fixture = Fixture::new();
    let services = fixture.services();
    assert!(fixture.update(&services).is_err());
    let stopped = fixture.installed(&services);
    let directory = storage::private_directory(&fixture.root.path().join("state"), false).unwrap();
    let lock =
        storage::open_private_file(&directory, Path::new("host-runtime.lock"), true).unwrap();
    let held = Lease::exclusive(lock).unwrap();
    let calls = services.runner.calls.borrow().len();
    assert_eq!(
        fixture.update(&services).err().unwrap().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(held);
    for from in ["0.16.0", "9.0.0", "01.0.0", "0.15.0-beta", "0.14.0"] {
        assert!(services
            .replace_stopped_with(
                &fixture.next,
                from,
                stopped.epoch,
                TrustedStage::authenticate_fixture_registered
            )
            .is_err());
    }
    std::fs::write(fixture.next.sessiond_path().unwrap(), b"changed").unwrap();
    assert!(fixture.update(&services).is_err());
    assert_eq!(services.runner.calls.borrow().len(), calls);
    assert!(services.replacement_status().unwrap().is_none());
}

#[test]
fn host_update_unknown_reload_retains_both_receipts_and_retries_only_original_target() {
    let fixture = Fixture::new();
    let services = fixture.services();
    let stopped = fixture.installed(&services);
    *services.runner.fail_reload.borrow_mut() = Some(*services.runner.reloads.borrow() + 1);
    assert!(fixture.update(&services).is_err());
    let pending = services.replacement_status().unwrap().unwrap();
    assert_eq!(pending.state, ReplacementState::Applying);
    assert_eq!(
        services.status().err().unwrap().to_string(),
        "host.update_recovery_required"
    );
    let recovered = fixture.update(&services).unwrap();
    assert_eq!(recovered.operation, pending.operation);
    assert_eq!(recovered.state, ReplacementState::Committed);
    assert_eq!(fixture.store.snapshot().unwrap(), stopped);
}

#[test]
fn host_update_partial_new_registration_remains_exact_owned_cleanup_after_revocation() {
    let fixture = Fixture::new();
    let services = fixture.services();
    fixture.installed(&services);
    *services.runner.fail_reload.borrow_mut() = Some(*services.runner.reloads.borrow() + 2);
    assert!(fixture.update(&services).is_err());
    assert_eq!(
        std::fs::read_dir(&services.context.registration)
            .unwrap()
            .count(),
        1
    );
    fixture.store.revoke().unwrap();
    let stopped = fixture.store.snapshot().unwrap();
    services.stop(stopped.epoch).unwrap();
    fixture
        .store
        .confirm_stopped(stopped.epoch, stopped.binding.as_ref().unwrap())
        .unwrap();
    fixture.store.request_cleanup().unwrap();
    let cleanup = fixture.store.snapshot().unwrap();
    assert_eq!(
        services.cleanup(cleanup.epoch).unwrap().process,
        ProcessState::Absent
    );
    assert_eq!(
        services.replacement_status().unwrap().unwrap().state,
        ReplacementState::Aborted
    );
    assert_eq!(
        std::fs::read_dir(&services.context.registration)
            .unwrap()
            .count(),
        0
    );
    assert_eq!(
        fixture.store.snapshot().unwrap().phase,
        Phase::CleanupPending
    );
}

#[test]
fn host_update_epoch_revocation_retains_unknown_then_explicit_stopped_retry_without_grant_restore()
{
    let fixture = Fixture::new();
    let services = fixture.services();
    fixture.installed(&services);
    let preserved = fixture.preserved();
    *services.runner.revoke_reload.borrow_mut() =
        Some((*services.runner.reloads.borrow() + 1, fixture.store.clone()));
    assert!(fixture.update(&services).is_err());
    assert_eq!(
        fixture.store.snapshot().unwrap().phase,
        Phase::StopRequested
    );
    let pending = services.replacement_status().unwrap().unwrap();
    let stopped = fixture.store.snapshot().unwrap();
    services.stop(stopped.epoch).unwrap();
    fixture
        .store
        .confirm_stopped(stopped.epoch, stopped.binding.as_ref().unwrap())
        .unwrap();
    let result = fixture.update(&services).unwrap();
    assert_eq!(result.operation, pending.operation);
    assert_eq!(fixture.preserved(), preserved);
    assert_eq!(fixture.store.snapshot().unwrap().phase, Phase::Disabled);
}

#[test]
fn host_update_changed_candidate_after_activation_retains_durable_unknown_without_repair() {
    let fixture = Fixture::new();
    let services = fixture.services();
    fixture.installed(&services);
    let helper = fixture.next.sessiond_path().unwrap().to_owned();
    *services.runner.tamper_reload.borrow_mut() =
        Some((*services.runner.reloads.borrow() + 2, helper.clone()));
    assert!(fixture.update(&services).is_err());
    assert_eq!(
        services.replacement_status().unwrap().unwrap().state,
        ReplacementState::Applying
    );
    assert!(fixture.update(&services).is_err());
    assert_eq!(std::fs::read(helper).unwrap(), b"changed helper");
    assert_eq!(
        services.registration().unwrap().1.unwrap().executable,
        fixture.old.path()
    );
}

#[test]
fn host_update_receipt_commit_failure_reopens_same_reserved_new_target_without_duplicate_registration(
) {
    let fixture = Fixture::new();
    let services = fixture.services();
    let stopped = fixture.installed(&services);
    let candidate_checks = std::cell::Cell::new(0);
    assert!(services
        .replace_stopped_with(&fixture.next, "0.15.0", stopped.epoch, |record, version| {
            TrustedStage::authenticate_fixture_registered(record, version)?;
            if version == crate::VERSION {
                candidate_checks.set(candidate_checks.get() + 1);
                if candidate_checks.get() == 2 {
                    return Err(refused("fixture.receipt_commit_interruption"));
                }
            }
            Ok(())
        },)
        .is_err());
    assert_eq!(
        services.registration().unwrap().1.unwrap().executable,
        fixture.next.path()
    );
    let pending = services.replacement_status().unwrap().unwrap();
    assert_eq!(pending.state, ReplacementState::Applying);
    assert_eq!(
        fixture.update(&services).unwrap().operation,
        pending.operation
    );
    assert_eq!(
        std::fs::read_dir(&services.context.registration)
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn host_update_reauthenticates_signed_old_and_new_provenance_without_cache_repair() {
    for candidate in [false, true] {
        let fixture = Fixture::new();
        let services = fixture.services();
        fixture.installed(&services);
        let provenance = if candidate {
            fixture.next.provenance_path()
        } else {
            fixture.old.provenance_path()
        };
        let signature = provenance.join(".manifest.json.sig");
        std::fs::write(&signature, b"tampered signature").unwrap();
        let before = services.runner.calls.borrow().len();
        assert!(fixture.update(&services).is_err());
        assert_eq!(services.runner.calls.borrow().len(), before);
        assert!(services.replacement_status().unwrap().is_none());
        assert_eq!(std::fs::read(signature).unwrap(), b"tampered signature");
    }
}

#[test]
#[ignore = "explicit owned daemon/helper paths required; no installed/native-manager acceptance"]
fn native_host_update_preserves_actual_persistent_daemon_pid_generation_and_original_helper() {
    use festerm_host_protocol::{
        AttachmentId, Creation, IssuedOperation, Lifetime, LogicalSessionId, PrincipalId, Recipe,
        Target,
    };
    use festerm_session::{Session, SessionEvent, SessionLifecycle};
    use festerm_sessiond::host_control::{ControlError, Lookup, SessionControl};
    use std::{
        sync::atomic::AtomicBool,
        time::{Duration, Instant},
    };

    fn retry<T>(mut operation: impl FnMut() -> Result<T, ControlError>) -> Result<T, ControlError> {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match operation() {
                Err(ControlError::Busy) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25))
                }
                outcome => return outcome,
            }
        }
    }

    fn exchange(frontend: &festerm_sessiond::PersistentSession, input: &[u8], expected: &[u8]) {
        frontend.try_send_input(input).unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut output = Vec::new();
        loop {
            if let Ok(SessionEvent::Output(bytes)) = frontend.try_recv_event() {
                output.extend_from_slice(&bytes);
                assert!(
                    output.len() <= 64 * 1024,
                    "owned frontend output exceeded bound"
                );
                if output
                    .windows(expected.len())
                    .any(|window| window == expected)
                {
                    return;
                }
            }
            assert!(Instant::now() < deadline, "owned frontend echo timed out");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    struct OwnedDaemon {
        control: SessionControl,
        owner: PrincipalId,
        target: Target,
        root: PathBuf,
    }
    impl Drop for OwnedDaemon {
        fn drop(&mut self) {
            let _ = retry(|| self.control.end(&self.owner, &self.target, |_| Ok(())));
            let deadline = Instant::now() + Duration::from_secs(8);
            while Instant::now() < deadline {
                if matches!(
                    retry(|| self
                        .control
                        .lookup(&self.owner, &self.target.logical, |_| Ok(()))),
                    Ok(Lookup::Absent)
                ) {
                    let _ = std::fs::remove_dir_all(&self.root);
                    return;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            eprintln!(
                "owned update daemon cleanup remains pending at {}",
                self.root.display()
            );
        }
    }

    let built_helper = PathBuf::from(
        std::env::var_os("FESTERM_ADR84_UPDATE_HELPER").expect("explicit owned helper"),
    );
    let child = built_helper.with_file_name("festerm-pty-test-child");
    let project = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    assert!(
        built_helper.starts_with(project.join("target"))
            && built_helper.is_file()
            && child.is_file()
    );
    // This is the current source binary in a synthetic old-version package,
    // not a genuine older release or native publisher-qualified executable.
    let helper_bytes = std::fs::read(&built_helper).unwrap();
    let fixture = Fixture::with_root_and_sessiond(
        storage::private_test_directory_in(project),
        Some(&helper_bytes),
    );
    let services = fixture.services();
    let stopped = fixture.installed(&services);
    let old_registration = services.registration().unwrap().1.unwrap();
    TrustedStage::authenticate_fixture_registered(&old_registration, "0.15.0").unwrap();
    let helper = fixture.old.sessiond_path().unwrap().to_owned();
    assert_ne!(helper, built_helper);
    assert_eq!(std::fs::read(&helper).unwrap(), helper_bytes);
    assert_eq!(old_registration.executable, fixture.old.path());
    assert_eq!(
        old_registration.provenance.as_deref(),
        Some(fixture.old.provenance_path())
    );
    let root = services.state.join("daemon");
    let control = SessionControl::with_helper(&root, &helper).unwrap();
    let owner = PrincipalId::new();
    let issued =
        IssuedOperation::new(retry(|| control.issuance_epoch(&owner, |_| Ok(()))).unwrap());
    let creation = Creation {
        logical: LogicalSessionId::new(),
        attachment: AttachmentId::new(),
        name: None,
        recipe: Recipe {
            shell: child.to_str().map(str::to_owned),
            args: [
                "read-line",
                "echo:before-update",
                "read-line",
                "echo:after-update",
                "spin",
            ]
            .map(str::to_owned)
            .into(),
            cwd: root.to_str().map(str::to_owned),
        },
        lifetime: Lifetime::Persistent {},
    };
    let created = retry(|| {
        control.ensure(
            &owner,
            &issued,
            &creation,
            false,
            &AtomicBool::new(false),
            |_| Ok(()),
        )
    })
    .unwrap();
    let daemon = OwnedDaemon {
        control,
        owner,
        target: created.target.clone(),
        root,
    };
    let pending = retry(|| {
        daemon.control.attach(
            &daemon.owner,
            &daemon.target,
            &creation.attachment,
            false,
            2 * 1024 * 1024,
            festerm_session::noop_session_event_notifier(),
            |_| Ok(()),
        )
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while pending.session.take_recovered_terminal_deferred().is_none() {
        assert!(
            Instant::now() < deadline,
            "owned frontend recovery timed out"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    pending.session.acknowledge_recovery_adoption();
    assert!(retry(|| daemon
        .control
        .attachment_outcome(&pending.authority, |_| Ok(())))
    .unwrap()
    .is_none());
    assert_eq!(
        pending.session.native_identity(),
        Some(created.identity.clone())
    );
    exchange(
        &pending.session,
        b"owned-before\n",
        b"before-update:owned-before",
    );
    let frontend_id = pending.session.id();
    let deadline = Instant::now() + Duration::from_secs(8);
    let before = loop {
        let running = festerm_sessiond::list_running_sessions_in(&daemon.root).unwrap();
        if running.len() == 1
            && running[0].attached_by == Some(festerm_sessiond::AttachKind::Remote)
        {
            break running;
        }
        assert!(Instant::now() < deadline, "owned frontend was not observed");
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].session.pid, created.identity.pid);
    assert_eq!(
        before[0].attached_by,
        Some(festerm_sessiond::AttachKind::Remote)
    );
    let control_record = std::fs::read(daemon.root.join("host-sessions.json")).unwrap();
    let preserved = fixture.preserved();

    let survives = || -> Result<(), &'static str> {
        if fixture.old.sessiond_path().map_err(|_| "old-helper")? != helper
            || std::fs::read(&helper).map_err(|_| "old-helper")? != helper_bytes
        {
            return Err("old-helper");
        }
        if festerm_sessiond::list_running_sessions_in(&daemon.root).map_err(|_| "daemon")? != before
            || !matches!(
                retry(|| daemon.control.lookup(&daemon.owner, &creation.logical, |_| Ok(()))),
                Ok(Lookup::Running(target)) if target == daemon.target
            )
        {
            return Err("daemon");
        }
        if pending.session.id() != frontend_id
            || pending.session.lifecycle() != SessionLifecycle::Running
            || pending.session.native_identity() != Some(created.identity.clone())
            || retry(|| {
                daemon
                    .control
                    .attachment_outcome(&pending.authority, |_| Ok(()))
            })
            .is_err()
            || std::fs::read(daemon.root.join("host-sessions.json")).map_err(|_| "control")?
                != control_record
        {
            return Err("frontend/control");
        }
        Ok(())
    };
    assert_eq!(survives(), Ok(()));
    let report = fixture.update(&services).unwrap();
    assert_eq!(report.state, ReplacementState::Committed);
    assert_eq!(
        report.previous_provenance.as_deref(),
        Some(fixture.old.provenance_path())
    );
    assert_eq!(
        services.registration().unwrap().1.unwrap().executable,
        fixture.next.path()
    );
    assert_eq!(fixture.store.snapshot().unwrap(), stopped);
    assert_eq!(fixture.preserved(), preserved);
    assert_eq!(survives(), Ok(()));
    exchange(
        &pending.session,
        b"owned-after\n",
        b"after-update:owned-after",
    );
    assert_eq!(survives(), Ok(()));
    let capabilities = std::process::Command::new(&helper)
        .args(["capabilities", "--json"])
        .env("FESTERM_SESSIOND_RUNTIME_ROOT", &daemon.root)
        .output()
        .unwrap();
    assert!(capabilities.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&capabilities.stdout).unwrap(),
        serde_json::to_value(festerm_sessiond::capabilities()).unwrap()
    );

    // A living mapped process alone cannot detect lost/replaced cached bytes.
    let retained = helper.with_extension("retained");
    std::fs::rename(&helper, &retained).unwrap();
    assert_eq!(survives(), Err("old-helper"));
    std::fs::write(&helper, b"wrong replacement helper").unwrap();
    assert_eq!(survives(), Err("old-helper"));
    std::fs::remove_file(&helper).unwrap();
    std::fs::rename(&retained, &helper).unwrap();
    assert_eq!(survives(), Ok(()));
    retry(|| {
        daemon
            .control
            .end(&daemon.owner, &daemon.target, |_| Ok(()))
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while !matches!(
        retry(|| daemon
            .control
            .lookup(&daemon.owner, &creation.logical, |_| Ok(()))),
        Ok(Lookup::Absent)
    ) {
        assert!(Instant::now() < deadline, "owned daemon did not end");
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(survives(), Err("daemon"));
    drop(pending);
    let daemon_root = daemon.root.clone();
    drop(daemon);
    assert!(
        !daemon_root.exists(),
        "exact owned daemon cleanup must finish"
    );
}

#[test]
fn host_update_missing_mutex_or_corrupt_journal_never_recreates_execution_authority() {
    let fixture = Fixture::new();
    let services = fixture.services();
    fixture.installed(&services);
    let state = fixture.root.path().join("state");
    std::fs::write(state.join("host-runtime.json"), b"retained history").unwrap();
    assert_eq!(
        fixture.update(&services).err().unwrap().to_string(),
        "host.runtime_lock_missing_reconciliation_required"
    );
    assert!(!state.join("host-runtime.lock").exists());
    assert!(!state.join(JOURNAL).exists());

    let fixture = Fixture::new();
    let services = fixture.services();
    fixture.installed(&services);
    let state = fixture.root.path().join("state");
    *services.runner.fail_reload.borrow_mut() = Some(*services.runner.reloads.borrow() + 1);
    assert!(fixture.update(&services).is_err());
    let journal = std::fs::read(state.join(JOURNAL)).unwrap();
    std::fs::write(state.join(JOURNAL), b"corrupt journal").unwrap();
    assert!(services.replacement_status().is_err());
    assert_eq!(
        std::fs::read(state.join(JOURNAL)).unwrap(),
        b"corrupt journal"
    );
    std::fs::write(state.join(JOURNAL), journal).unwrap();
    std::fs::remove_file(state.join(RECORD)).unwrap();
    std::fs::remove_file(state.join(LOCK)).unwrap();
    assert_eq!(
        services.replacement_status().err().unwrap().to_string(),
        "host.service_lock_missing_reconciliation_required"
    );
    assert!(!state.join(LOCK).exists());
}
