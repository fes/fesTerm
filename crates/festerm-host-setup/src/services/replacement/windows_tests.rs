use super::*;
use crate::enrollment::{owner_key_fingerprint, OwnerStorage};
use festerm_tunnel_bridge::{host_identity::HostIdentity, owner_auth::OwnerSigningKey};
use std::{cell::RefCell, collections::BTreeMap, sync::Arc};

#[derive(Default)]
struct Scheduler {
    tasks: BTreeMap<String, (String, String, i32)>,
    calls: Vec<NativeRequest>,
    fail_create_before: bool,
    fail_create_after: bool,
    fail_remove_after: bool,
    revoke_create: bool,
    fail_query: bool,
}

struct Model {
    state: RefCell<Scheduler>,
    store: Arc<Store>,
    directory: PathBuf,
}

impl CommandRunner for Model {
    fn execute(&self, request: &NativeRequest) -> io::Result<NativeOutput> {
        let NativeRequest::Task { name, action } = request else {
            panic!("Windows update must not launch a process")
        };
        assert!(name.len() <= 96);
        assert!(!matches!(action, TaskAction::Start | TaskAction::Stop));
        let mut state = self.state.borrow_mut();
        state.calls.push(request.clone());
        let security = "task-owner-system-full-control-v1:S-1-5-21-84".to_owned();
        let observed = match action {
            TaskAction::Prepare(xml) => TaskSnapshot {
                xml: Some(xml.clone()),
                security: Some(security),
                state: 0,
                last_exit: 0,
            },
            TaskAction::Create(xml) => {
                if self.directory.join(JOURNAL).exists() {
                    let journal: Replacement =
                        serde_json::from_slice(&std::fs::read(self.directory.join(JOURNAL))?)?;
                    let active: Registration =
                        serde_json::from_slice(&std::fs::read(self.directory.join(RECORD))?)?;
                    assert_eq!(journal.state, ReplacementState::Applying);
                    assert_eq!(journal.new.resource, *name);
                    assert!(journal.new.native_submitted);
                    assert!(journal.new.native_snapshot.is_none());
                    assert_eq!(
                        active, journal.old,
                        "old active receipt changed before Create"
                    );
                    assert_eq!(journal.new.native_expected.as_ref(), Some(xml));
                }
                if state.fail_create_before {
                    state.fail_create_before = false;
                    return Err(io::Error::other("fixture.create_before_unknown"));
                }
                assert!(!state.tasks.contains_key(name), "no create-or-update");
                state
                    .tasks
                    .insert(name.clone(), (xml.clone(), security.clone(), 3));
                if state.revoke_create {
                    state.revoke_create = false;
                    self.store.revoke()?;
                }
                if state.fail_create_after {
                    state.fail_create_after = false;
                    return Err(io::Error::other("fixture.create_after_unknown"));
                }
                TaskSnapshot {
                    xml: Some(xml.clone()),
                    security: Some(security),
                    state: 3,
                    last_exit: 0,
                }
            }
            TaskAction::Remove => {
                let (_, _, task_state) = state.tasks.get(name).expect("remove exact existing task");
                assert!(matches!(task_state, 1 | 3));
                state.tasks.remove(name);
                if state.fail_remove_after {
                    state.fail_remove_after = false;
                    return Err(io::Error::other("fixture.remove_after_unknown"));
                }
                TaskSnapshot {
                    xml: None,
                    security: None,
                    state: 0,
                    last_exit: 0,
                }
            }
            TaskAction::Query => {
                if state.fail_query {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "fixture.query_busy",
                    ));
                }
                match state.tasks.get(name) {
                    Some((xml, security, task_state)) => TaskSnapshot {
                        xml: Some(xml.clone()),
                        security: Some(security.clone()),
                        state: *task_state,
                        last_exit: 0,
                    },
                    None => TaskSnapshot {
                        xml: None,
                        security: None,
                        state: 0,
                        last_exit: 0,
                    },
                }
            }
            _ => unreachable!(),
        };
        Ok(NativeOutput {
            success: true,
            code: 0,
            stdout: serde_json::to_vec(&observed)?,
        })
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
        let root = storage::private_test_directory();
        let store = Arc::new(
            Store::create(
                &root.path().join("state"),
                Origin::Headless,
                Settings {
                    friendly_name: "Windows update model".into(),
                    allow_creation: false,
                    startup: Startup::Manual,
                },
            )
            .unwrap(),
        );
        let directory = storage::private_directory(&root.path().join("state"), false).unwrap();
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
        owners
            .approve_permissions(
                enrolled.revision,
                true,
                festerm_host_protocol::Capabilities {
                    inventory: true,
                    attach: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let old = trusted::version_fixture(root.path(), "0.15.0");
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
                platform: Platform::Windows,
                user: "S-1-5-21-84".into(),
                registration: self.root.path().join("unused-definitions"),
            },
            runner: Model {
                state: RefCell::new(Scheduler::default()),
                store: self.store.clone(),
                directory: self.root.path().join("state"),
            },
        }
    }

    fn installed(&self, services: &Services<'_, Model>) -> Snapshot {
        let before = self.store.snapshot().unwrap();
        services.install_reserved(&self.old, before.epoch).unwrap();
        self.store.revoke().unwrap();
        let stopped = self.store.snapshot().unwrap();
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

    fn preserved(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let mut result = Vec::new();
        for entry in std::fs::read_dir(self.root.path().join("state")).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap();
            if path.is_file()
                && !name.starts_with("native-service")
                && !name.starts_with("host-runtime")
                && !name.starts_with("host-lifecycle")
            {
                result.push((path.clone(), std::fs::read(path).unwrap()));
            }
        }
        for stage in [&self.old, &self.next] {
            let directory = stage.path().parent().unwrap();
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_file() {
                    result.push((path.clone(), std::fs::read(path).unwrap()));
                }
            }
        }
        result.sort();
        result
    }
}

fn creates(services: &Services<'_, Model>) -> usize {
    services
        .runner
        .state
        .borrow()
        .calls
        .iter()
        .filter(|request| {
            matches!(
                request,
                NativeRequest::Task {
                    action: TaskAction::Create(_),
                    ..
                }
            )
        })
        .count()
}

#[test]
fn windows_stopped_update_commits_exact_task_and_preserves_bundle_policy_and_grants() {
    let fixture = Fixture::new();
    let services = fixture.services();
    let snapshot = fixture.installed(&services);
    let preserved = fixture.preserved();
    let old = services.registration().unwrap().1.unwrap();
    let report = fixture.update(&services).unwrap();
    assert_eq!(report.state, ReplacementState::Committed);
    assert_eq!(fixture.store.snapshot().unwrap(), snapshot);
    assert_eq!(fixture.preserved(), preserved);
    let journal = services.replacement().unwrap().unwrap();
    let current = services.registration().unwrap().1.unwrap();
    assert_eq!(current, journal.new);
    assert_eq!(journal.old, old);
    assert_ne!(old.native_expected, current.native_expected);
    assert_ne!(old.resource, current.resource);
    assert!(current.resource.len() <= 96);
    assert!(current.native_submitted && current.native_snapshot.is_some());
    assert_eq!(services.observe(&old).unwrap(), ProcessState::Absent);
    assert_eq!(services.observe(&current).unwrap(), ProcessState::Stopped);
    let count = creates(&services);
    assert_eq!(
        fixture.update(&services).unwrap().operation,
        report.operation
    );
    assert_eq!(creates(&services), count);
}

#[test]
fn windows_stopped_update_recovers_same_submitted_task_without_duplicate_create() {
    let fixture = Fixture::new();
    let services = fixture.services();
    fixture.installed(&services);
    services.runner.state.borrow_mut().fail_create_after = true;
    assert!(fixture.update(&services).is_err());
    let pending = services.replacement().unwrap().unwrap();
    assert!(pending.new.native_submitted && pending.new.native_snapshot.is_none());
    assert_eq!(services.registration().unwrap().1.unwrap(), pending.old);
    let count = creates(&services);
    let report = fixture.update(&services).unwrap();
    assert_eq!(report.operation, pending.operation);
    assert_eq!(creates(&services), count);
}

#[test]
fn windows_stopped_update_submitted_unobserved_absence_stays_unknown() {
    let fixture = Fixture::new();
    let services = fixture.services();
    fixture.installed(&services);
    services.runner.state.borrow_mut().fail_create_before = true;
    assert!(fixture.update(&services).is_err());
    let pending = services.replacement().unwrap().unwrap();
    let count = creates(&services);
    assert_eq!(
        fixture.update(&services).err().unwrap().to_string(),
        "host.service_task_registration_outcome_unknown"
    );
    fixture.store.revoke().unwrap();
    let revoked = fixture.store.snapshot().unwrap();
    assert!(services.stop(revoked.epoch).is_err());
    assert_eq!(services.replacement().unwrap().unwrap(), pending);
    assert_eq!(creates(&services), count);
}

#[test]
fn windows_stopped_update_rejects_foreign_xml_acl_and_uncertain_query() {
    for field in 0..3 {
        let fixture = Fixture::new();
        let services = fixture.services();
        fixture.installed(&services);
        services.runner.state.borrow_mut().fail_create_after = true;
        assert!(fixture.update(&services).is_err());
        let pending = services.replacement().unwrap().unwrap();
        let mut state = services.runner.state.borrow_mut();
        match field {
            0 => state
                .tasks
                .get_mut(&pending.new.resource)
                .unwrap()
                .0
                .push_str("foreign"),
            1 => state
                .tasks
                .get_mut(&pending.new.resource)
                .unwrap()
                .1
                .push_str("foreign"),
            _ => state.fail_query = true,
        }
        drop(state);
        let count = creates(&services);
        assert!(fixture.update(&services).is_err());
        assert_eq!(services.replacement().unwrap().unwrap(), pending);
        assert_eq!(creates(&services), count);
    }
}

#[test]
fn windows_stopped_update_exact_retry_after_old_retirement_failure() {
    let fixture = Fixture::new();
    let services = fixture.services();
    fixture.installed(&services);
    services.runner.state.borrow_mut().fail_remove_after = true;
    assert!(fixture.update(&services).is_err());
    let pending = services.replacement().unwrap().unwrap();
    assert_eq!(pending.state, ReplacementState::Applying);
    assert!(!pending.new.native_submitted);
    assert_eq!(
        fixture.update(&services).unwrap().operation,
        pending.operation
    );
}

#[test]
fn windows_stopped_update_receipt_commit_failure_reuses_original_operation() {
    let fixture = Fixture::new();
    let services = fixture.services();
    let snapshot = fixture.installed(&services);
    let calls = std::cell::Cell::new(0);
    assert!(services
        .replace_stopped_with(
            &fixture.next,
            "0.15.0",
            snapshot.epoch,
            |record, version| {
                TrustedStage::authenticate_fixture_registered(record, version)?;
                if version == crate::VERSION {
                    calls.set(calls.get() + 1);
                    if calls.get() == 2 {
                        return Err(io::Error::other("fixture.final_auth_interruption"));
                    }
                }
                Ok(())
            }
        )
        .is_err());
    let pending = services.replacement().unwrap().unwrap();
    assert_eq!(services.registration().unwrap().1.unwrap(), pending.new);
    assert_eq!(pending.state, ReplacementState::Applying);
    let count = creates(&services);
    assert_eq!(
        fixture.update(&services).unwrap().operation,
        pending.operation
    );
    assert_eq!(creates(&services), count);
}

#[cfg(all(feature = "cloud", feature = "host-runtime"))]
#[test]
fn windows_stopped_update_revocation_retains_owned_completion_and_requires_both_tasks() {
    let fixture = Fixture::new();
    let services = fixture.services();
    fixture.installed(&services);
    {
        let mut state = services.runner.state.borrow_mut();
        state.revoke_create = true;
        state.fail_create_after = true;
    }
    assert!(fixture.update(&services).is_err());
    let mut pending = services.replacement().unwrap().unwrap();
    assert!(pending.new.native_snapshot.is_none());
    let revoked = fixture.store.snapshot().unwrap();
    services.reconcile_replacement_tasks(&mut pending).unwrap();
    assert!(pending.new.native_snapshot.is_some());
    assert_eq!(services.registration().unwrap().1.unwrap(), pending.old);
    for resource in [&pending.old, &pending.new] {
        let mut state = services.runner.state.borrow_mut();
        state.tasks.insert(
            resource.resource.clone(),
            (
                resource.native_snapshot.clone().unwrap(),
                resource.native_security.clone().unwrap(),
                4,
            ),
        );
        drop(state);
        assert_eq!(
            services
                .wait_windows_resources_revocation(
                    &revoked,
                    &[&pending.old, &pending.new],
                    std::time::Duration::ZERO,
                )
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        services
            .runner
            .state
            .borrow_mut()
            .tasks
            .get_mut(&resource.resource)
            .unwrap()
            .2 = 3;
    }
    let directory = storage::private_directory(&fixture.root.path().join("state"), false).unwrap();
    let runtime =
        storage::open_private_file(&directory, Path::new("host-runtime.lock"), true).unwrap();
    let lease = Lease::exclusive(runtime).unwrap();
    assert!(services
        .wait_windows_resources_revocation(
            &revoked,
            &[&pending.old, &pending.new],
            std::time::Duration::ZERO,
        )
        .is_err());
    drop(lease);
    assert!(services.stop(revoked.epoch).is_ok());
    assert_eq!(fixture.store.snapshot().unwrap(), revoked);
}

#[test]
fn windows_stopped_update_missing_active_receipt_and_tampered_bundle_do_not_imply_absence() {
    for missing in [false, true] {
        let fixture = Fixture::new();
        let services = fixture.services();
        fixture.installed(&services);
        services.runner.state.borrow_mut().fail_create_after = true;
        assert!(fixture.update(&services).is_err());
        let pending = services.replacement().unwrap().unwrap();
        if missing {
            std::fs::remove_file(fixture.root.path().join("state").join(RECORD)).unwrap();
        } else {
            std::fs::write(fixture.next.sessiond_path().unwrap(), b"changed helper").unwrap();
        }
        let count = creates(&services);
        assert!(fixture.update(&services).is_err());
        assert_eq!(services.replacement().unwrap().unwrap(), pending);
        assert_eq!(creates(&services), count);
    }
}

#[test]
fn windows_stopped_update_stale_epoch_live_runtime_and_publisher_refusal_precede_effects() {
    let fixture = Fixture::new();
    let services = fixture.services();
    let snapshot = fixture.installed(&services);
    let count = services.runner.state.borrow().calls.len();
    for epoch in [snapshot.epoch - 1, snapshot.epoch] {
        assert!(services
            .replace_stopped_with(&fixture.next, "0.15.0", epoch, |_, _| Err(refused(
                "fixture.publisher_refusal"
            )),)
            .is_err());
    }
    let directory = storage::private_directory(&fixture.root.path().join("state"), false).unwrap();
    let runtime =
        storage::open_private_file(&directory, Path::new("host-runtime.lock"), true).unwrap();
    let _lease = Lease::exclusive(runtime).unwrap();
    assert!(fixture.update(&services).is_err());
    assert_eq!(services.runner.state.borrow().calls.len(), count);
    assert!(services.replacement().unwrap().is_none());
}

#[test]
fn windows_stopped_update_compact_names_bind_both_uuids_and_origin() {
    let fixture = Fixture::new();
    let mut snapshot = fixture.store.snapshot().unwrap();
    for origin in [Origin::Desktop, Origin::Headless] {
        snapshot.origin = origin;
        let operation = uuid::Uuid::new_v4().to_string();
        let name = definitions::replacement_resource(
            &snapshot,
            snapshot.epoch,
            &operation,
            Platform::Windows,
        )
        .unwrap();
        assert!(name.len() <= 96);
        assert!(name.contains(
            &uuid::Uuid::parse_str(&snapshot.installation)
                .unwrap()
                .simple()
                .to_string()
        ));
        assert!(name.ends_with(
            &uuid::Uuid::parse_str(&operation)
                .unwrap()
                .simple()
                .to_string()
        ));
    }
}

#[test]
fn windows_stopped_update_cleanup_untrusted_tampered_or_missing_bundles_confirms_exact_absence() {
    for payload in ["untrusted", "tampered", "missing"] {
        let fixture = Fixture::new();
        let services = fixture.services();
        fixture.installed(&services);
        services.runner.state.borrow_mut().fail_create_after = true;
        assert!(fixture.update(&services).is_err());
        fixture.store.revoke().unwrap();
        let stopped = fixture.store.snapshot().unwrap();
        services.stop(stopped.epoch).unwrap();
        fixture
            .store
            .confirm_stopped(stopped.epoch, stopped.binding.as_ref().unwrap())
            .unwrap();
        fixture.store.request_cleanup().unwrap();
        let cleanup = fixture.store.snapshot().unwrap();
        let mut pending = services.replacement().unwrap().unwrap();
        for (target, version) in [
            (&pending.old, pending.from_version.as_str()),
            (&pending.new, pending.to_version.as_str()),
        ] {
            match payload {
                "tampered" => std::fs::write(&target.executable, b"untrusted payload").unwrap(),
                "missing" => std::fs::remove_file(&target.executable).unwrap(),
                _ => {}
            }
            // Test-key provenance is not trusted by production admission. This
            // is not a native publisher-revocation or offline PKI fixture.
            assert!(TrustedStage::authenticate_registered(target, version).is_err());
            if payload != "untrusted" {
                assert!(TrustedStage::authenticate_fixture_registered(target, version).is_err());
            }
        }
        let preserved = fixture.preserved();
        let mut active = services.registration().unwrap().1.unwrap();
        let count = services.runner.state.borrow().calls.len();
        assert!(services.cleanup(cleanup.epoch - 1).is_err());
        assert_eq!(services.runner.state.borrow().calls.len(), count);
        assert_eq!(services.replacement().unwrap().unwrap(), pending);
        let result = services.cleanup(cleanup.epoch).unwrap();
        assert_eq!(result.process, ProcessState::Absent);
        assert!(services.runner.state.borrow().tasks.is_empty());
        pending.state = ReplacementState::Aborted;
        assert_eq!(services.replacement().unwrap().unwrap(), pending);
        active.retiring_definition = true;
        active.removed = true;
        assert_eq!(services.registration().unwrap().1.unwrap(), active);
        assert_eq!(fixture.store.snapshot().unwrap(), cleanup);
        assert_eq!(fixture.preserved(), preserved);
    }
}

#[test]
fn windows_stopped_update_cleanup_foreign_uncertain_or_live_tasks_refuse_without_remove() {
    for fault in 0..5 {
        let fixture = Fixture::new();
        let services = fixture.services();
        fixture.installed(&services);
        {
            let mut state = services.runner.state.borrow_mut();
            state.fail_create_before = fault == 2;
            state.fail_create_after = fault != 2;
        }
        assert!(fixture.update(&services).is_err());
        fixture.store.request_cleanup().unwrap();
        let cleanup = fixture.store.snapshot().unwrap();
        let pending = services.replacement().unwrap().unwrap();
        let active = services.registration().unwrap().1.unwrap();
        let preserved = fixture.preserved();
        let mut state = services.runner.state.borrow_mut();
        match fault {
            0 => state
                .tasks
                .get_mut(&pending.new.resource)
                .unwrap()
                .0
                .push_str("foreign"),
            1 => state
                .tasks
                .get_mut(&pending.new.resource)
                .unwrap()
                .1
                .push_str("foreign"),
            2 => assert!(!state.tasks.contains_key(&pending.new.resource)),
            3 => state.fail_query = true,
            _ => state.tasks.get_mut(&pending.new.resource).unwrap().2 = 4,
        }
        let count = state.calls.len();
        drop(state);
        assert!(services.cleanup(cleanup.epoch).is_err());
        assert!(services.runner.state.borrow().calls[count..]
            .iter()
            .all(|request| matches!(
                request,
                NativeRequest::Task {
                    action: TaskAction::Query,
                    ..
                }
            )));
        let retained = services.replacement().unwrap().unwrap();
        if fault == 4 {
            let mut observed = pending;
            observed.new.native_snapshot = observed.new.native_expected.clone();
            assert_eq!(retained, observed);
        } else {
            assert_eq!(retained, pending);
        }
        assert_eq!(services.registration().unwrap().1.unwrap(), active);
        assert_eq!(fixture.store.snapshot().unwrap(), cleanup);
        assert_eq!(fixture.preserved(), preserved);
    }
}

#[test]
fn windows_stopped_update_cleanup_lost_new_remove_retries_only_exact_operation() {
    let fixture = Fixture::new();
    let services = fixture.services();
    fixture.installed(&services);
    services.runner.state.borrow_mut().fail_create_after = true;
    assert!(fixture.update(&services).is_err());
    fixture.store.revoke().unwrap();
    let stopped = fixture.store.snapshot().unwrap();
    services.stop(stopped.epoch).unwrap();
    fixture
        .store
        .confirm_stopped(stopped.epoch, stopped.binding.as_ref().unwrap())
        .unwrap();
    fixture.store.request_cleanup().unwrap();
    let cleanup = fixture.store.snapshot().unwrap();
    let mut pending = services.replacement().unwrap().unwrap();
    let active = services.registration().unwrap().1.unwrap();
    let preserved = fixture.preserved();
    let count = services.runner.state.borrow().calls.len();
    services.runner.state.borrow_mut().fail_remove_after = true;
    assert!(services.cleanup(cleanup.epoch).is_err());
    assert_eq!(services.replacement().unwrap().unwrap(), pending);
    assert_eq!(services.registration().unwrap().1.unwrap(), active);
    assert!(services.runner.state.borrow().tasks.is_empty());
    assert_eq!(fixture.store.snapshot().unwrap(), cleanup);
    assert_eq!(fixture.preserved(), preserved);
    assert_eq!(
        services.cleanup(cleanup.epoch).unwrap().process,
        ProcessState::Absent
    );
    pending.state = ReplacementState::Aborted;
    assert_eq!(services.replacement().unwrap().unwrap(), pending);
    let state = services.runner.state.borrow();
    let removed: Vec<_> = state.calls[count..]
        .iter()
        .filter_map(|request| match request {
            NativeRequest::Task {
                name,
                action: TaskAction::Remove,
            } => Some(name.as_str()),
            NativeRequest::Task {
                action: TaskAction::Query,
                ..
            } => None,
            _ => panic!("cleanup must only query/remove exact tasks"),
        })
        .collect();
    assert_eq!(removed, [pending.new.resource.as_str()]);
    assert_eq!(fixture.store.snapshot().unwrap(), cleanup);
    assert_eq!(fixture.preserved(), preserved);
}

#[test]
fn windows_stopped_update_missing_receipt_cannot_finalize_aborted_cleanup() {
    let fixture = Fixture::new();
    let services = fixture.services();
    fixture.installed(&services);
    services.runner.state.borrow_mut().fail_create_after = true;
    assert!(fixture.update(&services).is_err());
    fixture.store.revoke().unwrap();
    let stopped = fixture.store.snapshot().unwrap();
    services.stop(stopped.epoch).unwrap();
    fixture
        .store
        .confirm_stopped(stopped.epoch, stopped.binding.as_ref().unwrap())
        .unwrap();
    fixture.store.request_cleanup().unwrap();
    std::fs::remove_file(fixture.root.path().join("state").join(RECORD)).unwrap();
    let epoch = fixture.store.snapshot().unwrap().epoch;
    assert!(services.cleanup(epoch).is_err());
    assert_eq!(
        services.replacement().unwrap().unwrap().state,
        ReplacementState::Applying
    );
    assert!(services.registration().unwrap().1.is_none());
}

#[test]
fn windows_stopped_update_v3_receipt_rejects_foreign_fences_before_scheduler_effects() {
    for field in 0..6 {
        let fixture = Fixture::new();
        let services = fixture.services();
        let snapshot = fixture.installed(&services);
        services.runner.state.borrow_mut().fail_create_after = true;
        assert!(fixture.update(&services).is_err());
        let mut pending = services.replacement().unwrap().unwrap();
        match field {
            0 => pending.new.binding.account = "github:other".into(),
            1 => pending.new.origin = Origin::Desktop,
            2 => pending.new.epoch = snapshot.epoch + 1,
            3 => pending.new.resource.push_str("-foreign"),
            4 => pending.new.native_security = Some("foreign".into()),
            _ => pending.new.native_snapshot = Some("foreign".into()),
        }
        std::fs::write(
            fixture.root.path().join("state").join(JOURNAL),
            serde_json::to_vec(&pending).unwrap(),
        )
        .unwrap();
        let count = services.runner.state.borrow().calls.len();
        assert!(fixture.update(&services).is_err());
        assert!(services.replacement_status().is_err());
        assert_eq!(services.runner.state.borrow().calls.len(), count);
    }
}
