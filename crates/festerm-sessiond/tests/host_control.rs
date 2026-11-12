#![cfg(unix)]

use festerm_host_protocol::{
    AttachmentId, Creation, DetachKind, GenerationId, IssuedOperation, Lifetime, LogicalSessionId,
    OperationId, PrincipalId, Recipe, Target,
};
use festerm_session::noop_session_event_notifier;
use festerm_sessiond::{
    host_control::{
        AdmissionState, AttachmentAuthority, ControlError, Effect, Lookup, PendingAttachment,
        SessionControl,
    },
    host_lease::DaemonWatch,
    list_running_sessions_in, private_storage as storage,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Barrier,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Fixture {
    root: PathBuf,
    owner: PrincipalId,
}

fn allow(_: &AdmissionState) -> Result<(), ControlError> {
    Ok(())
}

fn control_result_diagnostic<T>(result: &Result<T, ControlError>) -> &'static str {
    match result {
        Ok(_) => "Ok",
        Err(ControlError::Denied) => "Err(Denied)",
        Err(ControlError::Revoked) => "Err(Revoked)",
        Err(ControlError::Unknown) => "Err(Unknown)",
        Err(ControlError::Busy) => "Err(Busy)",
        Err(ControlError::Stale) => "Err(Stale)",
        Err(ControlError::RecipeConflict) => "Err(RecipeConflict)",
        Err(ControlError::OperationConflict) => "Err(OperationConflict)",
        Err(ControlError::AlreadyExists) => "Err(AlreadyExists)",
        Err(ControlError::Ended) => "Err(Ended)",
        Err(ControlError::Cancelled) => "Err(Cancelled)",
        Err(ControlError::Invalid) => "Err(Invalid)",
        Err(ControlError::Storage) => "Err(Storage)",
        Err(ControlError::CommittedButNotPublished) => "Err(CommittedButNotPublished)",
    }
}

#[test]
fn host_control_result_diagnostics_are_content_free_and_variant_exact() {
    struct Opaque;

    for (error, expected) in [
        (ControlError::Denied, "Err(Denied)"),
        (ControlError::Revoked, "Err(Revoked)"),
        (ControlError::Unknown, "Err(Unknown)"),
        (ControlError::Busy, "Err(Busy)"),
        (ControlError::Stale, "Err(Stale)"),
        (ControlError::RecipeConflict, "Err(RecipeConflict)"),
        (ControlError::OperationConflict, "Err(OperationConflict)"),
        (ControlError::AlreadyExists, "Err(AlreadyExists)"),
        (ControlError::Ended, "Err(Ended)"),
        (ControlError::Cancelled, "Err(Cancelled)"),
        (ControlError::Invalid, "Err(Invalid)"),
        (ControlError::Storage, "Err(Storage)"),
        (
            ControlError::CommittedButNotPublished,
            "Err(CommittedButNotPublished)",
        ),
    ] {
        let result: Result<Opaque, ControlError> = Err(error);
        assert_eq!(control_result_diagnostic(&result), expected);
    }
    assert_eq!(
        control_result_diagnostic(&Ok::<_, ControlError>(Opaque)),
        "Ok"
    );
    assert_eq!(
        control_result_diagnostic(&Ok::<_, ControlError>("private-payload-sentinel")),
        "Ok"
    );
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

fn eventually(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "owned daemon condition timed out"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn retry_busy<T>(
    mut operation: impl FnMut() -> Result<T, ControlError>,
) -> Result<T, ControlError> {
    let mut outcome = None;
    eventually(|| match operation() {
        Err(ControlError::Busy) => false,
        result => {
            outcome = Some(result);
            true
        }
    });
    outcome.unwrap()
}

impl Fixture {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!(
                "hc-{}",
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ));
        storage::private_directory(&root, true).unwrap();
        if std::env::var_os("FESTERM_HOST_TEST_DIAGNOSTICS").is_some() {
            use std::os::unix::fs::PermissionsExt;
            let helper = Path::new(env!("CARGO_BIN_EXE_festerm-sessiond"));
            let wrapper = root.join("owned-captured-helper");
            let quoted = |path: &Path| path.to_str().unwrap().replace('\'', "'\\''");
            fs::write(
                &wrapper,
                format!(
                    "#!/bin/sh\numask 077\nexport FESTERM_SESSIOND_TRACE_FILE='{}'\nexec '{}' \"$@\" >>'{}' 2>&1\n",
                    quoted(&root.join("owned-native-trace.log")),
                    quoted(helper),
                    quoted(&root.join("owned-native-start.log")),
                ),
            )
            .unwrap();
            fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self {
            root,
            owner: PrincipalId::new(),
        }
    }

    fn control(&self) -> SessionControl {
        let captured = self.root.join("owned-captured-helper");
        SessionControl::with_helper(
            &self.root,
            if captured.is_file() {
                &captured
            } else {
                Path::new(env!("CARGO_BIN_EXE_festerm-sessiond"))
            },
        )
        .unwrap()
    }

    fn issued(&self) -> IssuedOperation {
        IssuedOperation::new(
            retry_busy(|| self.control().issuance_epoch(&self.owner, allow)).unwrap(),
        )
    }

    fn failing_control(&self) -> (SessionControl, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let helper = self.root.join("owned-failing-helper");
        let marker = self.root.join("helper-called");
        fs::write(
            &helper,
            format!("#!/bin/sh\nprintf called >'{}'\nexit 1\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        (
            SessionControl::with_helper(&self.root, &helper).unwrap(),
            marker,
        )
    }

    fn state(&self) -> serde_json::Value {
        let directory = storage::private_directory(&self.root, false).unwrap();
        serde_json::from_slice(
            &storage::read_private(&directory, Path::new("host-sessions.json"), 4 * 1024 * 1024)
                .unwrap(),
        )
        .unwrap()
    }

    fn save_state(&self, state: &serde_json::Value) {
        let directory = storage::private_directory(&self.root, false).unwrap();
        storage::replace_private(
            &directory,
            Path::new("host-sessions.json"),
            &serde_json::to_vec(state).unwrap(),
        )
        .unwrap();
    }

    fn creation(&self, lifetime: Lifetime) -> Creation {
        let child = Path::new(env!("CARGO_BIN_EXE_festerm-sessiond"))
            .with_file_name("festerm-pty-test-child");
        assert!(
            child.is_file(),
            "build the repository fixture: cargo build -p festerm-pty-test-child"
        );
        Creation {
            logical: LogicalSessionId::new(),
            attachment: AttachmentId::new(),
            name: None,
            recipe: Recipe {
                shell: child.to_str().map(str::to_owned),
                args: vec!["spin".into()],
                cwd: self.root.to_str().map(str::to_owned),
            },
            lifetime,
        }
    }
}

#[test]
fn reconciliation_without_launch_journal_never_executes_helper_even_with_active_lease() {
    let fixture = Fixture::new();
    let creation = fixture.creation(Lifetime::Persistent {});
    let operation = fixture.issued();
    let (control, marker) = fixture.failing_control();
    assert!(matches!(
        control.ensure(
            &fixture.owner,
            &operation,
            &creation,
            false,
            &AtomicBool::new(false),
            allow
        ),
        Err(ControlError::Unknown)
    ));
    assert!(marker.exists());
    fs::remove_file(&marker).unwrap();
    let launch = fixture.root.join(format!("launch-{}.json", operation.id));
    fs::remove_file(&launch).unwrap();
    let state = fixture.state();
    let generation = state["sessions"][creation.logical.as_str()]["generation"]
        .as_str()
        .unwrap()
        .parse::<u128>()
        .unwrap();
    let lease: serde_json::Value = serde_json::from_slice(
        &fs::read(fixture.root.join(format!("host-daemon-{generation}.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(lease["stopped"], false);
    assert_eq!(lease["deadline_ms"], serde_json::Value::Null);
    assert_eq!(
        control
            .lookup(&fixture.owner, &creation.logical, allow)
            .unwrap(),
        Lookup::Unknown
    );
    let inventory = control.inventory(&fixture.owner, allow).unwrap();
    assert!(inventory.sessions.is_empty());
    assert_eq!(inventory.unknown.len(), 1);
    for (cancelled, deny) in [(true, false), (false, true)] {
        assert!(control
            .ensure(
                &fixture.owner,
                &operation,
                &creation,
                false,
                &AtomicBool::new(cancelled),
                |_| if deny {
                    Err(ControlError::Denied)
                } else {
                    Ok(())
                },
            )
            .is_err());
    }
    let mut revoked = fixture.state();
    revoked["sessions"][creation.logical.as_str()]["revoked"] = true.into();
    fixture.save_state(&revoked);
    assert!(matches!(
        control.lookup(&fixture.owner, &creation.logical, allow),
        Err(ControlError::Revoked)
    ));
    let inventory = control.inventory(&fixture.owner, allow).unwrap();
    assert!(inventory.sessions.is_empty() && inventory.unknown.is_empty());
    assert!(
        !marker.exists(),
        "reconciliation invoked the creation helper"
    );
    assert!(
        !launch.exists(),
        "reconciliation wrote a new launch journal"
    );
    assert!(list_running_sessions_in(&fixture.root).unwrap().is_empty());
}

#[test]
fn transient_native_observation_preserves_bound_running_phase_and_revision() {
    let fixture = Fixture::new();
    let creation = fixture.creation(Lifetime::Persistent {});
    let running = fixture.ensure(&creation);
    let before = fixture.state();
    let directory = storage::private_directory(&fixture.root, false).unwrap();
    let registry =
        storage::read_private(&directory, Path::new("registry.json"), 4 * 1024 * 1024).unwrap();
    storage::replace_private(&directory, Path::new("registry.json"), b"invalid").unwrap();
    let observed = fixture
        .control()
        .lookup(&fixture.owner, &creation.logical, allow);
    storage::replace_private(&directory, Path::new("registry.json"), &registry).unwrap();
    assert!(matches!(observed, Err(ControlError::Unknown)));
    assert_eq!(fixture.state()["sessions"], before["sessions"]);
    assert_eq!(
        fixture
            .control()
            .lookup(&fixture.owner, &creation.logical, allow)
            .unwrap(),
        Lookup::Running(running.target)
    );
}

#[test]
fn operation_capacity_rotates_scopes_without_resurrection_or_resume_outage() {
    use festerm_host_protocol::MAX_IDEMPOTENCY_RECORDS;
    let fixture = Fixture::new();
    let creation = fixture.creation(Lifetime::Persistent {});
    let original = fixture.issued();
    let running = fixture
        .control()
        .ensure(
            &fixture.owner,
            &original,
            &creation,
            false,
            &AtomicBool::new(false),
            allow,
        )
        .unwrap();
    for _ in 0..MAX_IDEMPOTENCY_RECORDS + 2 {
        assert_eq!(fixture.ensure(&creation).target, running.target);
    }
    assert!(fixture.state()["operations"].as_object().unwrap().len() <= MAX_IDEMPOTENCY_RECORDS);
    assert!(fixture.state()["issuance_epoch"].as_u64().unwrap() > original.epoch.get());
    assert!(
        !fs::read_dir(&fixture.root).unwrap().flatten().any(|entry| {
            let name = entry.file_name();
            name.to_string_lossy().starts_with("host-receipt-")
                || name.to_string_lossy().starts_with("host-retired-")
        })
    );
    assert!(matches!(
        fixture.control().ensure(
            &fixture.owner,
            &original,
            &creation,
            false,
            &AtomicBool::new(false),
            allow
        ),
        Err(ControlError::Stale)
    ));
    fixture
        .control()
        .end(&fixture.owner, &running.target, allow)
        .unwrap();
    fixture.wait_absent(&running.target);
    let replacement = fixture.ensure(&creation);
    assert!(replacement.recreated);
    assert_ne!(replacement.target.generation, running.target.generation);
    assert!(matches!(
        fixture.control().ensure(
            &fixture.owner,
            &original,
            &creation,
            false,
            &AtomicBool::new(false),
            allow
        ),
        Err(ControlError::Stale)
    ));
    let mut changed = creation.clone();
    changed.recipe.args.push("changed".into());
    assert!(matches!(
        fixture.control().ensure(
            &fixture.owner,
            &original,
            &changed,
            false,
            &AtomicBool::new(false),
            allow
        ),
        Err(ControlError::Stale)
    ));
}

#[test]
fn logical_capacity_retires_absent_records_but_never_erases_revocation_or_generation_fences() {
    use festerm_host_protocol::MAX_SESSIONS;
    let fixture = Fixture::new();
    let mut creation = fixture.creation(Lifetime::Persistent {});
    creation.logical = "ffffffff-ffff-4fff-8fff-ffffffffffff".parse().unwrap();
    let running = fixture.ensure(&creation);
    fixture
        .control()
        .end(&fixture.owner, &running.target, allow)
        .unwrap();
    fixture.wait_absent(&running.target);
    let mut state = fixture.state();
    let original = state["sessions"][creation.logical.as_str()].clone();
    let other_owner = PrincipalId::new();
    let mut revoked = None;
    for index in 1..MAX_SESSIONS {
        let logical: LogicalSessionId = format!("{index:08x}-0000-4000-8000-000000000001")
            .parse()
            .unwrap();
        let mut record = original.clone();
        record["logical"] = logical.as_str().into();
        record["name"] = format!("hs-slot-{index:02}").into();
        record["generation"] = serde_json::json!(GenerationId::new(
            running.target.generation.get() + index as u128
        )
        .unwrap());
        record["identity"] = serde_json::Value::Null;
        record["artifacts_retired"] = true.into();
        record["launch"] = serde_json::json!(OperationId::new());
        if index >= MAX_SESSIONS / 2 {
            record["owner"] = serde_json::json!(other_owner);
        }
        record["revoked"] = (index == 1).into();
        if index == 1 {
            revoked = Some(logical.clone());
        }
        state["sessions"][logical.as_str()] = record;
    }
    state["last_generation"] =
        serde_json::json!(running.target.generation.get() + MAX_SESSIONS as u128);
    fixture.save_state(&state);
    let foreign_before: Vec<_> = state["sessions"]
        .as_object()
        .unwrap()
        .values()
        .filter(|record| record["owner"] == serde_json::json!(other_owner))
        .cloned()
        .collect();
    let next = fixture.creation(Lifetime::Persistent {});
    let retired_scope = fixture.issued();
    assert!(matches!(
        fixture.control().ensure(
            &fixture.owner,
            &retired_scope,
            &next,
            false,
            &AtomicBool::new(false),
            allow
        ),
        Err(ControlError::Stale)
    ));
    assert_eq!(
        fixture.state()["sessions"].as_object().unwrap().len(),
        MAX_SESSIONS - 1
    );
    let foreign_after: Vec<_> = fixture.state()["sessions"]
        .as_object()
        .unwrap()
        .values()
        .filter(|record| record["owner"] == serde_json::json!(other_owner))
        .cloned()
        .collect();
    assert_eq!(foreign_before, foreign_after);
    let replacement = fixture.ensure(&next);
    assert!(replacement.target.generation > running.target.generation);
    assert_eq!(
        fixture.state()["sessions"].as_object().unwrap().len(),
        MAX_SESSIONS
    );
    assert_eq!(
        fixture
            .control()
            .lookup(&fixture.owner, &creation.logical, allow)
            .unwrap(),
        Lookup::Absent
    );
    assert!(matches!(
        fixture
            .control()
            .lookup(&fixture.owner, &revoked.unwrap(), allow),
        Err(ControlError::Revoked)
    ));
    let recreated = fixture.ensure(&creation);
    assert!(recreated.recreated);
    assert!(recreated.target.generation > replacement.target.generation);
    assert!(matches!(
        fixture.control().ensure(
            &fixture.owner,
            &retired_scope,
            &next,
            false,
            &AtomicBool::new(false),
            allow
        ),
        Err(ControlError::Stale)
    ));
}

#[test]
fn issuance_rotation_reauthorizes_actual_epoch_before_durable_publication() {
    struct Guard {
        root: PathBuf,
        epoch: u64,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            let directory = storage::private_directory(&self.root, false).unwrap();
            let bytes =
                storage::read_private(&directory, Path::new("host-sessions.json"), 4 * 1024 * 1024)
                    .unwrap();
            let state: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(state["issuance_epoch"].as_u64(), Some(self.epoch));
        }
    }
    let fixture = Fixture::new();
    fixture.ensure(&fixture.creation(Lifetime::Persistent {}));
    let mut state = fixture.state();
    let old = state["issuance_epoch"].as_u64().unwrap();
    let mut operation = state["operations"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone();
    operation["created"] = false.into();
    for _ in state["operations"].as_object().unwrap().len()
        ..festerm_host_protocol::MAX_IDEMPOTENCY_RECORDS
    {
        state["operations"][OperationId::new().as_str()] = operation.clone();
    }
    fixture.save_state(&state);
    assert_eq!(
        fixture.control().issuance_epoch(&fixture.owner, |state| {
            if state.issuance_epoch.unwrap().get() == old {
                Ok(())
            } else {
                Err(ControlError::Denied)
            }
        }),
        Err(ControlError::Denied)
    );
    assert_eq!(fixture.state(), state);
    let mut captured = Vec::new();
    let returned = fixture
        .control()
        .issuance_epoch(&fixture.owner, |state| {
            let epoch = state.issuance_epoch.unwrap().get();
            captured.push(epoch);
            Ok(Guard {
                root: fixture.root.clone(),
                epoch,
            })
        })
        .unwrap();
    assert_eq!(captured, [old, returned.get()]);
    assert_eq!(returned.get(), old + 1);
    assert!(fixture.state()["operations"]
        .as_object()
        .unwrap()
        .is_empty());
}

#[test]
fn local_pending_retirement_denies_foreign_owner_under_journal_and_native_contention() {
    let fixture = Fixture::new();
    let (control, _) = fixture.failing_control();
    let creation = fixture.creation(Lifetime::Persistent {});
    let operation = fixture.issued();
    assert!(matches!(
        control.ensure(
            &fixture.owner,
            &operation,
            &creation,
            false,
            &AtomicBool::new(false),
            allow
        ),
        Err(ControlError::Unknown)
    ));
    let before = fixture.state();
    let record = &before["sessions"][creation.logical.as_str()];
    let target = Target {
        logical: creation.logical.clone(),
        generation: serde_json::from_value(record["generation"].clone()).unwrap(),
    };
    assert_eq!(record["phase"], "Launching");
    assert!(record["identity"].is_null());
    let directory = storage::private_directory(&fixture.root, false).unwrap();
    let lease_name = PathBuf::from(format!("host-daemon-{}.json", target.generation.get()));
    let lease_before = storage::read_private(&directory, &lease_name, 4096).unwrap();
    let lease: serde_json::Value = serde_json::from_slice(&lease_before).unwrap();
    assert!(lease["native_pid"].is_null());
    let launch_name = PathBuf::from(format!("launch-{}.json", operation.id.as_str()));
    let launch_before = storage::read_private(&directory, &launch_name, 4096).unwrap();
    let start =
        festerm_sessiond::lock_native_start_in(&fixture.root, record["name"].as_str().unwrap())
            .unwrap();
    let registry =
        storage::open_private_file(&directory, Path::new("registry.lock"), false).unwrap();
    fs2::FileExt::try_lock_exclusive(&registry).unwrap();
    let journal =
        storage::open_private_file(&directory, Path::new("host-sessions.lock"), false).unwrap();
    fs2::FileExt::try_lock_exclusive(&journal).unwrap();
    let foreign = PrincipalId::new();
    assert_eq!(
        control.retire_local(&foreign, &target),
        Err(ControlError::Denied),
        "readable protected ownership must deny before any retirement effect"
    );
    assert_eq!(
        control.retire_local(&fixture.owner, &target),
        Err(ControlError::Busy)
    );
    assert_eq!(
        control.retire_local(
            &foreign,
            &Target {
                logical: LogicalSessionId::new(),
                generation: target.generation,
            },
        ),
        Err(ControlError::Denied)
    );
    assert_eq!(fixture.state(), before);
    assert_eq!(
        storage::read_private(&directory, &lease_name, 4096).unwrap(),
        lease_before
    );
    assert_eq!(
        storage::read_private(&directory, &launch_name, 4096).unwrap(),
        launch_before
    );
    let checkpoint =
        storage::read_private(&directory, Path::new("host-sessions.json"), 4 * 1024 * 1024)
            .unwrap();
    storage::replace_private(&directory, Path::new("host-sessions.json"), b"invalid").unwrap();
    for owner in [&foreign, &fixture.owner] {
        assert_eq!(
            control.retire_local(owner, &target),
            Err(ControlError::Storage),
            "a corrupt checkpoint cannot supply an ownership decision"
        );
    }
    assert_eq!(
        &**storage::read_private(&directory, Path::new("host-sessions.json"), 7).unwrap(),
        b"invalid"
    );
    assert_eq!(
        storage::read_private(&directory, &lease_name, 4096).unwrap(),
        lease_before
    );
    storage::replace_private(&directory, Path::new("host-sessions.json"), &checkpoint).unwrap();
    drop(journal);
    assert_eq!(
        control.retire_local(&foreign, &target),
        Err(ControlError::Denied)
    );
    assert_eq!(
        control.retire_local(&fixture.owner, &target),
        Err(ControlError::Busy),
        "native start ownership is not positive absence"
    );
    assert_eq!(fixture.state(), before);
    assert_eq!(
        storage::read_private(&directory, &lease_name, 4096).unwrap(),
        lease_before
    );
    drop(start);
    assert_eq!(
        control.retire_local(&fixture.owner, &target),
        Err(ControlError::Unknown),
        "unreadable native registry is not positive absence"
    );
    assert_eq!(fixture.state(), before);
    let sealed: serde_json::Value =
        serde_json::from_slice(&storage::read_private(&directory, &lease_name, 4096).unwrap())
            .unwrap();
    assert_eq!(sealed["native_pid"], lease["native_pid"]);
    assert_eq!(sealed["generation"], lease["generation"]);
    assert_eq!(sealed["grant"], lease["grant"]);
    assert_eq!(sealed["stopped"], true);
    assert_eq!(
        storage::read_private(&directory, &launch_name, 4096).unwrap(),
        launch_before
    );
    drop(registry);
}

#[test]
fn local_pending_retirement_requires_positive_native_absence_and_fences_old_scopes() {
    use festerm_host_protocol::MAX_PENDING_CREATES;
    let fixture = Fixture::new();
    let (control, marker) = fixture.failing_control();
    let mut pending = Vec::new();
    for _ in 0..MAX_PENDING_CREATES {
        let creation = fixture.creation(Lifetime::Persistent {});
        let operation = fixture.issued();
        assert!(matches!(
            control.ensure(
                &fixture.owner,
                &operation,
                &creation,
                false,
                &AtomicBool::new(false),
                allow
            ),
            Err(ControlError::Unknown)
        ));
        let state = fixture.state();
        let generation: GenerationId = serde_json::from_value(
            state["sessions"][creation.logical.as_str()]["generation"].clone(),
        )
        .unwrap();
        pending.push((creation, operation, generation));
    }
    let next = fixture.creation(Lifetime::Persistent {});
    assert!(matches!(
        fixture.control().ensure(
            &fixture.owner,
            &fixture.issued(),
            &next,
            false,
            &AtomicBool::new(false),
            allow
        ),
        Err(ControlError::Busy)
    ));
    fs::remove_file(&marker).unwrap();
    let interrupted = fixture.state();
    let first = &pending[0];
    let first_target = Target {
        logical: first.0.logical.clone(),
        generation: first.2,
    };
    let directory = storage::private_directory(&fixture.root, false).unwrap();
    storage::replace_private(&directory, Path::new("registry.json"), b"invalid").unwrap();
    assert_eq!(
        control.retire_local(&fixture.owner, &first_target),
        Err(ControlError::Unknown)
    );
    assert_eq!(fixture.state(), interrupted);
    assert_eq!(
        fixture.state()["sessions"].as_object().unwrap().len(),
        MAX_PENDING_CREATES
    );
    fs::remove_file(fixture.root.join("registry.json")).unwrap();
    for (creation, _, generation) in &pending {
        let target = Target {
            logical: creation.logical.clone(),
            generation: *generation,
        };
        let before_denial = fixture.state();
        let lease_name = PathBuf::from(format!("host-daemon-{}.json", generation.get()));
        let lease_before = storage::read_private(&directory, &lease_name, 4096).unwrap();
        assert_eq!(
            control.retire_local(&PrincipalId::new(), &target),
            Err(ControlError::Denied)
        );
        assert_eq!(fixture.state(), before_denial);
        assert_eq!(
            storage::read_private(&directory, &lease_name, 4096).unwrap(),
            lease_before
        );
        control.retire_local(&fixture.owner, &target).unwrap();
        assert!(DaemonWatch::open(
            &fixture.root,
            interrupted["sessions"][creation.logical.as_str()]["name"]
                .as_str()
                .unwrap(),
            generation.get()
        )
        .is_err());
    }
    let running = fixture.ensure(&next);
    assert_eq!(fixture.state()["sessions"].as_object().unwrap().len(), 1);
    for (creation, operation, _) in &pending {
        assert_eq!(
            control
                .lookup(&fixture.owner, &creation.logical, allow)
                .unwrap(),
            Lookup::Absent
        );
        assert!(matches!(
            control.ensure(
                &fixture.owner,
                operation,
                creation,
                false,
                &AtomicBool::new(false),
                allow
            ),
            Err(ControlError::Stale)
        ));
    }
    assert!(!marker.exists());
    assert_eq!(list_running_sessions_in(&fixture.root).unwrap().len(), 1);
    assert_eq!(
        fixture
            .control()
            .lookup(&fixture.owner, &next.logical, allow)
            .unwrap(),
        Lookup::Running(running.target)
    );
}

#[test]
fn interrupted_retirement_keeps_original_operation_noncreating_after_artifact_removal() {
    let fixture = Fixture::new();
    let (control, marker) = fixture.failing_control();
    let creation = fixture.creation(Lifetime::Persistent {});
    let operation = fixture.issued();
    assert!(matches!(
        control.ensure(
            &fixture.owner,
            &operation,
            &creation,
            false,
            &AtomicBool::new(false),
            allow
        ),
        Err(ControlError::Unknown)
    ));
    let mut before_rotation = fixture.state();
    let record = &mut before_rotation["sessions"][creation.logical.as_str()];
    let generation: GenerationId = serde_json::from_value(record["generation"].clone()).unwrap();
    let target = Target {
        logical: creation.logical.clone(),
        generation,
    };
    control.retire_local(&fixture.owner, &target).unwrap();
    record["phase"] = "Absent".into();
    record["artifacts_retired"] = true.into();
    record["epoch"] = (record["epoch"].as_u64().unwrap() + 1).into();
    fixture.save_state(&before_rotation);
    fs::remove_file(&marker).unwrap();
    assert!(matches!(
        control.ensure(
            &fixture.owner,
            &operation,
            &creation,
            false,
            &AtomicBool::new(false),
            allow
        ),
        Err(ControlError::Ended)
    ));
    assert!(!marker.exists());
    assert!(list_running_sessions_in(&fixture.root).unwrap().is_empty());
    let recreated = fixture.ensure(&creation);
    assert!(recreated.recreated);
    assert!(recreated.target.generation > generation);
}

impl Fixture {
    #[track_caller]
    fn ensure(&self, creation: &Creation) -> festerm_sessiond::host_control::EnsureResult {
        let issued = self.issued();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match self.control().ensure(
                &self.owner,
                &issued,
                creation,
                false,
                &AtomicBool::new(false),
                allow,
            ) {
                Err(ControlError::Busy) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(20))
                }
                Ok(result) => return result,
                Err(error) => panic!(
                    "owned ensure failed: {error}; record={:?}; native={:?}",
                    self.state()["sessions"].get(creation.logical.as_str()),
                    list_running_sessions_in(&self.root)
                ),
            }
        }
    }

    fn edit_lease(&self, target: &Target, edit: impl FnOnce(&mut serde_json::Value)) {
        let directory = storage::private_directory(&self.root, false).unwrap();
        let name = PathBuf::from(format!("host-daemon-{}.json", target.generation.get()));
        let bytes = storage::read_private(&directory, &name, 4096).unwrap();
        let mut lease: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        lease["revision"] = (lease["revision"].as_u64().unwrap() + 1).into();
        edit(&mut lease);
        storage::replace_private(&directory, &name, &serde_json::to_vec(&lease).unwrap()).unwrap();
    }

    fn wait_absent(&self, target: &Target) {
        eventually(|| {
            matches!(
                self.control().lookup(&self.owner, &target.logical, allow),
                Ok(Lookup::Absent)
            )
        });
    }

    fn adopt(
        &self,
        _target: &Target,
        pending: PendingAttachment,
    ) -> (
        festerm_sessiond::PersistentSession,
        Option<festerm_host_protocol::TemporaryLease>,
    ) {
        eventually(|| pending.session.take_recovered_terminal_deferred().is_some());
        pending.session.acknowledge_recovery_adoption();
        eventually(
            || match self.control().attachment_outcome(&pending.authority, allow) {
                Ok(_) => true,
                Err(ControlError::Busy | ControlError::Storage) => false,
                Err(error) => panic!("attachment commit failed: {error}"),
            },
        );
        (pending.session, pending.reserved_lease)
    }

    fn adopt_authorized(
        &self,
        target: &Target,
        pending: PendingAttachment,
    ) -> (
        festerm_sessiond::PersistentSession,
        Option<festerm_host_protocol::TemporaryLease>,
        AttachmentAuthority,
    ) {
        let authority = pending.authority.clone();
        let (session, lease) = self.adopt(target, pending);
        (session, lease, authority)
    }

    fn attach(
        &self,
        target: &Target,
        attachment: &AttachmentId,
        takeover: bool,
    ) -> PendingAttachment {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match self.control().attach(
                &self.owner,
                target,
                attachment,
                takeover,
                2 * 1024 * 1024,
                noop_session_event_notifier(),
                allow,
            ) {
                Err(ControlError::Busy) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(20))
                }
                result => return result.unwrap(),
            }
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if thread::panicking() {
            if let Ok(directory) = storage::private_directory(&self.root, false) {
                for entry in fs::read_dir(&self.root).unwrap().flatten() {
                    let name = entry.file_name();
                    if name.to_string_lossy().ends_with(".json")
                        || name == "owned-native-start.log"
                        || name == "owned-native-trace.log"
                    {
                        if let Ok(bytes) =
                            storage::read_private(&directory, Path::new(&name), 4 * 1024 * 1024)
                        {
                            eprintln!(
                                "OWNED FAILURE {}: {}",
                                entry.path().display(),
                                String::from_utf8_lossy(&bytes)
                            );
                        }
                    }
                }
            }
            if let Ok(directory) = storage::private_directory(&self.root.join("lifecycle"), false) {
                for entry in fs::read_dir(self.root.join("lifecycle")).unwrap().flatten() {
                    if entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "json")
                    {
                        if let Ok(bytes) = storage::read_private(
                            &directory,
                            Path::new(&entry.file_name()),
                            32 * 1024,
                        ) {
                            eprintln!(
                                "OWNED FAILURE {}: {}",
                                entry.path().display(),
                                String::from_utf8_lossy(&bytes)
                            );
                        }
                    }
                }
            }
        }
        let mut pids = Vec::new();
        if let Ok(directory) = storage::private_directory(&self.root.join("lifecycle"), false) {
            for entry in fs::read_dir(self.root.join("lifecycle")).unwrap().flatten() {
                if entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "json")
                {
                    if let Ok(bytes) =
                        storage::read_private(&directory, Path::new(&entry.file_name()), 256 * 1024)
                    {
                        if let Ok(entries) = serde_json::from_slice::<
                            Vec<festerm_sessiond::lifecycle::Entry>,
                        >(&bytes)
                        {
                            pids.extend(entries.into_iter().map(|entry| entry.pid));
                        }
                    }
                }
            }
        }
        // Only generation files in this newly created fixture root are touched.
        if let Ok(directory) = storage::private_directory(&self.root, false) {
            for entry in fs::read_dir(&self.root).unwrap().flatten() {
                let name = entry.file_name();
                if name.to_string_lossy().starts_with("host-daemon-")
                    && name.to_string_lossy().ends_with(".json")
                {
                    if let Ok(bytes) = storage::read_private(&directory, Path::new(&name), 4096) {
                        if let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                            if let Some(pid) = value["native_pid"].as_u64() {
                                pids.push(pid as u32);
                            }
                            value["stopped"] = true.into();
                            value["revision"] = (value["revision"].as_u64().unwrap() + 1).into();
                            let _ = storage::replace_private(
                                &directory,
                                Path::new(&name),
                                &serde_json::to_vec(&value).unwrap(),
                            );
                        }
                    }
                }
            }
        }
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match list_running_sessions_in(&self.root) {
                Ok(sessions)
                    if sessions.is_empty()
                        && pids
                            .iter()
                            .all(|pid| !festerm_sessiond::process_alive(*pid)) =>
                {
                    fs::remove_dir_all(&self.root).unwrap();
                    break;
                }
                _ if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
                _ => panic!(
                    "fixture daemon did not stop; retained {}",
                    self.root.display()
                ),
            }
        }
    }
}

#[test]
fn concurrent_ensure_lost_reply_and_recipe_conflict_use_one_real_daemon() {
    let fixture = Fixture::new();
    let creation = Arc::new(fixture.creation(Lifetime::Persistent {}));
    let operation = Arc::new(fixture.issued());
    let barrier = Arc::new(Barrier::new(6));
    let workers: Vec<_> = (0..6)
        .map(|index| {
            let control = fixture.control();
            let owner = fixture.owner.clone();
            let creation = creation.clone();
            let operation = if index % 2 == 0 {
                Arc::new(fixture.issued())
            } else {
                operation.clone()
            };
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    match control.ensure(
                        &owner,
                        &operation,
                        &creation,
                        false,
                        &AtomicBool::new(false),
                        allow,
                    ) {
                        Err(ControlError::Busy) if Instant::now() < deadline => {
                            thread::sleep(Duration::from_millis(20))
                        }
                        result => break result.unwrap(),
                    }
                }
            })
        })
        .collect();
    let joined: Vec<_> = workers.into_iter().map(|worker| worker.join()).collect();
    let results: Vec<_> = joined.into_iter().map(Result::unwrap).collect();
    assert!(results
        .iter()
        .all(|result| result.identity == results[0].identity));
    assert_eq!(list_running_sessions_in(&fixture.root).unwrap().len(), 1);
    assert!(list_running_sessions_in(&fixture.root).unwrap()[0]
        .attached_by
        .is_none());
    // Dropping/reopening the manager (lost reply/bridge restart) keeps the shell.
    let control = fixture.control();
    let replay = retry_busy(|| {
        control.ensure(
            &fixture.owner,
            &operation,
            &creation,
            false,
            &AtomicBool::new(false),
            allow,
        )
    });
    assert!(
        replay.is_ok(),
        "HOST control stage=concurrent-ensure/lost-reply actual={}",
        control_result_diagnostic(&replay),
    );
    let replay = replay.unwrap();
    assert_eq!(replay.identity, results[0].identity);
    let mut changed = (*creation).clone();
    changed.recipe.args.push("emit:not-run".into());
    let changed_operation = fixture.issued();
    let directory = storage::private_directory(&fixture.root, false).unwrap();
    let stem = format!("host-daemon-{}", replay.target.generation.get());
    let locked =
        storage::open_private_file(&directory, Path::new(&format!("{stem}.lock")), false).unwrap();
    eventually(|| match fs2::FileExt::try_lock_exclusive(&locked) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
        Err(_) => panic!("HOST control stage=concurrent-ensure/hold-generation-lease failed"),
    });
    let journal_before =
        storage::read_private(&directory, Path::new("host-sessions.json"), 4 * 1024 * 1024)
            .unwrap();
    let lease_name = PathBuf::from(format!("{stem}.json"));
    let lease_before = storage::read_private(&directory, &lease_name, 4096).unwrap();
    let held = control.ensure(
        &fixture.owner,
        &changed_operation,
        &changed,
        false,
        &AtomicBool::new(false),
        allow,
    );
    assert!(
        matches!(&held, Err(ControlError::Busy)),
        "HOST control stage=concurrent-ensure/held-generation-lease actual={}",
        control_result_diagnostic(&held),
    );
    assert!(
        storage::read_private(&directory, Path::new("host-sessions.json"), 4 * 1024 * 1024)
            .unwrap()
            == journal_before,
        "HOST control stage=concurrent-ensure/held-generation-lease journal-changed",
    );
    assert!(
        storage::read_private(&directory, &lease_name, 4096).unwrap() == lease_before,
        "HOST control stage=concurrent-ensure/held-generation-lease lease-changed",
    );
    let state_before: serde_json::Value = serde_json::from_slice(&journal_before).unwrap();
    drop(locked);
    let conflict = retry_busy(|| {
        control.ensure(
            &fixture.owner,
            &changed_operation,
            &changed,
            false,
            &AtomicBool::new(false),
            allow,
        )
    });
    assert!(
        matches!(&conflict, Err(ControlError::RecipeConflict)),
        "HOST control stage=concurrent-ensure/changed-recipe actual={}",
        control_result_diagnostic(&conflict),
    );
    let conflict = retry_busy(|| {
        control.ensure(
            &fixture.owner,
            &operation,
            &changed,
            false,
            &AtomicBool::new(false),
            allow,
        )
    });
    assert!(
        matches!(&conflict, Err(ControlError::OperationConflict)),
        "HOST control stage=concurrent-ensure/changed-operation-intent actual={}",
        control_result_diagnostic(&conflict),
    );
    let state_after = fixture.state();
    assert!(
        state_after["operations"] == state_before["operations"],
        "HOST control stage=concurrent-ensure/conflicts operation-journal-changed",
    );
    let logical = creation.logical.as_str();
    for field in [
        "recipe",
        "effective_recipe",
        "generation",
        "identity",
        "launch",
    ] {
        assert!(
            state_after["sessions"][logical][field] == state_before["sessions"][logical][field],
            "HOST control stage=concurrent-ensure/conflicts original-record-changed",
        );
    }
    let native = list_running_sessions_in(&fixture.root).unwrap();
    assert!(
        native.len() == 1,
        "HOST control stage=concurrent-ensure/conflicts daemon-count-changed",
    );
    assert!(
        native[0].session.name == replay.identity.name
            && native[0].session.pid == replay.identity.pid
            && native[0].session.created_at_unix_ms == replay.target.generation.get(),
        "HOST control stage=concurrent-ensure/conflicts original-daemon-changed",
    );
}

#[test]
fn crash_before_spawn_stays_unknown_and_after_spawn_adopts_only_reserved_generation() {
    let fixture = Fixture::new();
    let creation = fixture.creation(Lifetime::Persistent {});
    let operation = fixture.issued();
    let running = fixture
        .control()
        .ensure(
            &fixture.owner,
            &operation,
            &creation,
            false,
            &AtomicBool::new(false),
            allow,
        )
        .unwrap();
    let directory = storage::private_directory(&fixture.root, false).unwrap();
    let name = Path::new("host-sessions.json");
    let bytes = storage::read_private(&directory, name, 4 * 1024 * 1024).unwrap();
    let mut state: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let record = &mut state["sessions"][creation.logical.as_str()];
    record["phase"] = "Launching".into();
    record["identity"] = serde_json::Value::Null;
    storage::replace_private(&directory, name, &serde_json::to_vec(&state).unwrap()).unwrap();
    let adopted = fixture
        .control()
        .ensure(
            &fixture.owner,
            &operation,
            &creation,
            false,
            &AtomicBool::new(false),
            allow,
        )
        .unwrap();
    assert_eq!(adopted.identity, running.identity);

    fixture
        .control()
        .end(&fixture.owner, &running.target, allow)
        .unwrap();
    fixture.wait_absent(&running.target);
    // Restore the pre-spawn journal image with no native launch journal. This
    // is an intentionally ambiguous crash boundary, not permission to respawn.
    let launch = fixture.root.join(format!("launch-{}.json", operation.id));
    fs::remove_file(launch).unwrap();
    storage::replace_private(&directory, name, &serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(matches!(
        fixture.control().ensure(
            &fixture.owner,
            &operation,
            &creation,
            false,
            &AtomicBool::new(false),
            allow
        ),
        Err(ControlError::Unknown)
    ));
    assert!(list_running_sessions_in(&fixture.root).unwrap().is_empty());
}

#[test]
fn absence_denial_revocation_cancellation_and_generations_remain_distinct() {
    let fixture = Fixture::new();
    let creation = fixture.creation(Lifetime::Persistent {});
    assert_eq!(
        fixture
            .control()
            .lookup(&fixture.owner, &creation.logical, allow)
            .unwrap(),
        Lookup::Absent
    );
    let denied = fixture.control().ensure(
        &fixture.owner,
        &fixture.issued(),
        &creation,
        false,
        &AtomicBool::new(false),
        |_| Err::<(), _>(ControlError::Denied),
    );
    assert!(
        matches!(&denied, Err(ControlError::Denied)),
        "HOST control stage=absence-denial/initial-ensure actual={}",
        control_result_diagnostic(&denied),
    );
    let cancelled = AtomicBool::new(true);
    let operation = fixture.issued();
    assert!(matches!(
        retry_busy(|| fixture.control().ensure(
            &fixture.owner,
            &operation,
            &creation,
            false,
            &cancelled,
            allow
        )),
        Err(ControlError::Cancelled)
    ));
    cancelled.store(false, Ordering::Release);
    let running = fixture
        .control()
        .ensure(
            &fixture.owner,
            &fixture.issued(),
            &creation,
            false,
            &cancelled,
            |state| {
                if state.effect == Effect::Resume && state.grant.is_some() {
                    cancelled.store(true, Ordering::Release);
                }
                Ok(())
            },
        )
        .unwrap();
    assert!(running.cancelled);
    assert_eq!(list_running_sessions_in(&fixture.root).unwrap().len(), 1);
    let stale = Target {
        logical: creation.logical.clone(),
        generation: GenerationId::new(running.target.generation.get() + 1).unwrap(),
    };
    assert_eq!(
        fixture.control().end(&fixture.owner, &stale, allow),
        Err(ControlError::Stale)
    );
    fixture
        .control()
        .end(&fixture.owner, &running.target, allow)
        .unwrap();
    fixture.wait_absent(&running.target);
    let replacement = fixture.ensure(&creation);
    assert_ne!(replacement.target.generation, running.target.generation);
    assert_eq!(
        fixture
            .control()
            .end(&fixture.owner, &running.target, allow),
        Err(ControlError::Stale)
    );
    fixture
        .control()
        .revoke_local(&fixture.owner, &creation.logical)
        .unwrap();
    assert_eq!(
        fixture
            .control()
            .lookup(&fixture.owner, &creation.logical, allow),
        Err(ControlError::Revoked)
    );
    assert!(matches!(
        fixture.control().ensure(
            &fixture.owner,
            &fixture.issued(),
            &creation,
            false,
            &AtomicBool::new(false),
            allow
        ),
        Err(ControlError::Revoked)
    ));
    assert_eq!(list_running_sessions_in(&fixture.root).unwrap().len(), 1);
}

#[test]
fn incomplete_inventory_preserves_healthy_sessions_and_noncreating_unknown_targets() {
    let fixture = Fixture::new();
    let healthy = fixture.ensure(&fixture.creation(Lifetime::Persistent {}));
    let creation = fixture.creation(Lifetime::Persistent {});
    let uncertain = fixture.ensure(&creation);
    let before = fixture.state();
    let directory = storage::private_directory(&fixture.root, false).unwrap();
    let original =
        storage::read_private(&directory, Path::new("registry.json"), 4 * 1024 * 1024).unwrap();
    let mut registry: serde_json::Value = serde_json::from_slice(&original).unwrap();
    registry["sessions"][&uncertain.identity.name]["spawn_fingerprint"] = serde_json::Value::Null;
    storage::replace_private(
        &directory,
        Path::new("registry.json"),
        &serde_json::to_vec(&registry).unwrap(),
    )
    .unwrap();
    let inventory = fixture.control().inventory(&fixture.owner, allow).unwrap();
    assert_eq!(inventory.sessions.len(), 1);
    assert_eq!(inventory.sessions[0].summary.target, healthy.target);
    assert_eq!(inventory.unknown.len(), 1);
    assert_eq!(inventory.unknown[0].target, uncertain.target);
    assert_eq!(
        inventory.unknown[0].grant.state,
        festerm_host_protocol::LogicalState::Unknown
    );
    assert!(matches!(
        fixture
            .control()
            .lookup(&fixture.owner, &creation.logical, allow),
        Err(ControlError::Unknown)
    ));
    assert!(matches!(
        fixture.control().ensure(
            &fixture.owner,
            &fixture.issued(),
            &creation,
            false,
            &AtomicBool::new(false),
            allow
        ),
        Err(ControlError::Unknown)
    ));
    assert_eq!(fixture.state()["sessions"], before["sessions"]);
    storage::replace_private(&directory, Path::new("registry.json"), &original).unwrap();
    assert_eq!(list_running_sessions_in(&fixture.root).unwrap().len(), 2);
}

#[test]
fn per_generation_actions_and_heartbeats_do_not_take_global_journal_or_other_generation_locks() {
    let fixture = Fixture::new();
    let first_creation = fixture.creation(Lifetime::Temporary {
        reconnect_grace_seconds: 60,
    });
    let first = fixture.ensure(&first_creation);
    let pending = fixture.attach(&first.target, &first_creation.attachment, false);
    let (_first_session, _, first_authority) = fixture.adopt_authorized(&first.target, pending);
    let second_creation = fixture.creation(Lifetime::Temporary {
        reconnect_grace_seconds: 60,
    });
    let second = fixture.ensure(&second_creation);
    let pending = fixture.attach(&second.target, &second_creation.attachment, false);
    let (_second_session, _, second_authority) = fixture.adopt_authorized(&second.target, pending);
    let directory = storage::private_directory(&fixture.root, false).unwrap();
    let journal =
        storage::open_private_file(&directory, Path::new("host-sessions.lock"), false).unwrap();
    fs2::FileExt::try_lock_exclusive(&journal).unwrap();
    for authority in [&first_authority, &second_authority] {
        eventually(|| {
            fixture
                .control()
                .with_attachment_authorized(
                    authority,
                    |state| {
                        assert!(state.issuance_epoch.is_none());
                        Ok(())
                    },
                    || Ok(()),
                )
                .is_ok()
        });
        eventually(|| {
            fixture
                .control()
                .maintain_lease(authority, None, allow)
                .is_ok()
        });
    }
    drop(journal);
    let barrier = Arc::new(Barrier::new(2));
    let peer = barrier.clone();
    let control = fixture.control();
    let worker = thread::spawn(move || {
        peer.wait();
        for _ in 0..16 {
            eventually(|| {
                control
                    .with_attachment_authorized(&second_authority, allow, || Ok(()))
                    .is_ok()
            });
            eventually(|| {
                control
                    .maintain_lease(&second_authority, None, allow)
                    .is_ok()
            });
            thread::sleep(Duration::from_millis(2));
        }
    });
    barrier.wait();
    fixture
        .control()
        .revoke_local(&fixture.owner, &first.target.logical)
        .unwrap();
    assert!(matches!(
        fixture.control().with_attachment_authorized(
            &first_authority,
            allow,
            || -> Result<(), ControlError> { panic!("revoked action") }
        ),
        Err(ControlError::Revoked)
    ));
    worker.join().unwrap();
}

#[test]
fn revoked_live_generation_remains_accounted_and_requires_exact_local_end_and_reconsent() {
    let fixture = Fixture::new();
    let creation = fixture.creation(Lifetime::Persistent {});
    let running = fixture.ensure(&creation);
    let before_revoke = fixture.state();
    fixture
        .control()
        .revoke_local(&fixture.owner, &creation.logical)
        .unwrap();
    // Lease publication committed, but model a crash before journal publication.
    fixture.save_state(&before_revoke);
    let observation = fixture
        .control()
        .observe_local(&fixture.owner, &running.target)
        .unwrap();
    assert!(observation.revoked);
    assert_eq!(observation.state, Lookup::Running(running.target.clone()));
    assert_eq!(fixture.state()["sessions"].as_object().unwrap().len(), 1);
    assert_eq!(
        fixture
            .control()
            .retire_local(&fixture.owner, &running.target),
        Err(ControlError::Revoked)
    );
    assert_eq!(
        fixture
            .control()
            .reconfirm_local(&fixture.owner, &running.target, observation.revision),
        Err(ControlError::Busy)
    );
    assert_eq!(list_running_sessions_in(&fixture.root).unwrap().len(), 1);
    fixture
        .control()
        .end_local(&fixture.owner, &running.target)
        .unwrap();
    eventually(|| {
        fixture
            .control()
            .observe_local(&fixture.owner, &running.target)
            .is_ok_and(|value| value.state == Lookup::Absent)
    });
    let absent = fixture
        .control()
        .observe_local(&fixture.owner, &running.target)
        .unwrap();
    assert!(absent.revoked);
    assert_eq!(
        fixture
            .control()
            .reconfirm_local(&fixture.owner, &running.target, observation.revision),
        Err(ControlError::Stale)
    );
    assert_eq!(
        fixture
            .control()
            .retire_local(&fixture.owner, &running.target),
        Err(ControlError::Revoked)
    );
    let directory = storage::private_directory(&fixture.root, false).unwrap();
    let daemon = storage::open_private_file(
        &directory,
        Path::new(&format!("{}.daemon.lock", running.identity.name)),
        false,
    )
    .unwrap();
    retry_busy(|| match fs2::FileExt::try_lock_exclusive(&daemon) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Err(ControlError::Busy),
        Err(error) => panic!("owned daemon lifetime lock failed: {error}"),
    })
    .unwrap();
    assert_eq!(
        fixture
            .control()
            .reconfirm_local(&fixture.owner, &running.target, absent.revision),
        Err(ControlError::Busy)
    );
    drop(daemon);
    retry_busy(|| {
        fixture
            .control()
            .reconfirm_local(&fixture.owner, &running.target, absent.revision)
    })
    .unwrap();
    assert_eq!(
        fixture
            .control()
            .lookup(&fixture.owner, &creation.logical, allow)
            .unwrap(),
        Lookup::Absent
    );
    assert!(fixture.ensure(&creation).target.generation > running.target.generation);
}

#[test]
fn generation_churn_reuses_fixed_native_slot_and_reclaims_only_proven_dead_artifacts() {
    let fixture = Fixture::new();
    let creation = fixture.creation(Lifetime::Persistent {});
    let mut running = fixture.ensure(&creation);
    let name = running.identity.name.clone();
    for _ in 0..6 {
        retry_busy(|| {
            fixture
                .control()
                .end(&fixture.owner, &running.target, allow)
        })
        .unwrap();
        fixture.wait_absent(&running.target);
        let previous = running.target.generation;
        running = fixture.ensure(&creation);
        assert_eq!(running.identity.name, name);
        assert!(running.recreated);
        assert!(running.target.generation > previous);
        let names = fs::read_dir(&fixture.root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(!names
            .iter()
            .any(|name| name.contains(&previous.get().to_string())));
        assert_eq!(
            names
                .iter()
                .filter(|name| name.starts_with("launch-") && name.ends_with(".json"))
                .count(),
            1
        );
        assert_eq!(
            names
                .iter()
                .filter(|name| name.starts_with("host-daemon-") && name.ends_with(".json"))
                .count(),
            1
        );
        assert!(
            names.len() < 24,
            "unbounded generation artifacts: {names:?}"
        );
    }
}

#[test]
fn pending_retirement_seals_delayed_helpers_but_waits_for_native_lifetime_lock() {
    let fixture = Fixture::new();
    let (control, _) = fixture.failing_control();
    let creation = fixture.creation(Lifetime::Persistent {});
    assert!(matches!(
        control.ensure(
            &fixture.owner,
            &fixture.issued(),
            &creation,
            false,
            &AtomicBool::new(false),
            allow
        ),
        Err(ControlError::Unknown)
    ));
    let state = fixture.state();
    let record = &state["sessions"][creation.logical.as_str()];
    let target = Target {
        logical: creation.logical.clone(),
        generation: serde_json::from_value(record["generation"].clone()).unwrap(),
    };
    let directory = storage::private_directory(&fixture.root, false).unwrap();
    let name = record["name"].as_str().unwrap();
    let daemon =
        storage::open_private_file(&directory, Path::new(&format!("{name}.daemon.lock")), true)
            .unwrap();
    fs2::FileExt::try_lock_exclusive(&daemon).unwrap();
    assert_eq!(
        control.retire_local(&fixture.owner, &target),
        Err(ControlError::Busy)
    );
    let lease = storage::read_private(
        &directory,
        Path::new(&format!("host-daemon-{}.json", target.generation.get())),
        4096,
    )
    .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&lease).unwrap()["stopped"],
        true
    );
    assert_eq!(fixture.state()["sessions"].as_object().unwrap().len(), 1);
    drop(daemon);
    control.retire_local(&fixture.owner, &target).unwrap();
    assert!(DaemonWatch::open(&fixture.root, name, target.generation.get()).is_err());
}

#[test]
fn intentional_initial_cancellation_cannot_stop_pending_or_transferred_temporary_ownership() {
    let fixture = Fixture::new();
    let creation = fixture.creation(Lifetime::Temporary {
        reconnect_grace_seconds: 60,
    });
    let running = fixture.ensure(&creation);
    let authority = running.initial_lease_authority.as_ref().unwrap();
    let pending = fixture.attach(&running.target, &creation.attachment, false);
    assert_eq!(
        retry_busy(|| fixture.control().cancel_initial_lease(authority, allow)),
        Err(ControlError::Stale)
    );
    let (session, _, attached) = fixture.adopt_authorized(&running.target, pending);
    assert_eq!(
        retry_busy(|| fixture.control().cancel_initial_lease(authority, allow)),
        Err(ControlError::Stale)
    );
    fixture
        .control()
        .with_attachment_authorized(&attached, allow, || Ok(()))
        .unwrap();
    drop(session);
    let next = fixture.ensure(&fixture.creation(Lifetime::Temporary {
        reconnect_grace_seconds: 60,
    }));
    retry_busy(|| {
        fixture
            .control()
            .cancel_initial_lease(next.initial_lease_authority.as_ref().unwrap(), allow)
    })
    .unwrap();
    fixture.wait_absent(&next.target);
    assert_eq!(list_running_sessions_in(&fixture.root).unwrap().len(), 1);
}

#[test]
fn missing_or_unsupported_durable_journal_never_resets_issuance_or_infers_absence() {
    let fixture = Fixture::new();
    let creation = fixture.creation(Lifetime::Persistent {});
    let running = fixture.ensure(&creation);
    let original = fixture.state();
    fs::remove_file(fixture.root.join("host-sessions.json")).unwrap();
    assert_eq!(
        fixture
            .control()
            .lookup(&fixture.owner, &creation.logical, allow),
        Err(ControlError::Storage)
    );
    assert!(matches!(
        fixture.control().issuance_epoch(&fixture.owner, allow),
        Err(ControlError::Storage)
    ));
    assert!(!fixture.root.join("host-sessions.json").exists());
    fixture.save_state(&original);
    for field in ["last_generation", "issuance_epoch"] {
        let mut malformed = original.clone();
        malformed.as_object_mut().unwrap().remove(field);
        fixture.save_state(&malformed);
        assert!(matches!(
            fixture.control().issuance_epoch(&fixture.owner, allow),
            Err(ControlError::Storage)
        ));
    }
    let mut unsupported = original.clone();
    unsupported["version"] = 1.into();
    fixture.save_state(&unsupported);
    assert_eq!(
        fixture
            .control()
            .lookup(&fixture.owner, &creation.logical, allow),
        Err(ControlError::Storage)
    );
    fixture.save_state(&original);
    assert_eq!(
        fixture
            .control()
            .lookup(&fixture.owner, &creation.logical, allow)
            .unwrap(),
        Lookup::Running(running.target)
    );
    assert_eq!(list_running_sessions_in(&fixture.root).unwrap().len(), 1);
}

#[test]
fn persistent_host_shell_accepts_ordinary_local_unticketed_attachment() {
    let fixture = Fixture::new();
    let running = fixture.ensure(&fixture.creation(Lifetime::Persistent {}));
    let selected = list_running_sessions_in(&fixture.root)
        .unwrap()
        .into_iter()
        .find(|entry| entry.session.created_at_unix_ms == running.target.generation.get())
        .unwrap()
        .session;
    let local = festerm_sessiond::PersistentSession::resume_discovered_in(
        &selected,
        &fixture.root,
        noop_session_event_notifier(),
    )
    .unwrap();
    eventually(|| local.take_recovered_terminal_deferred().is_some());
    assert!(list_running_sessions_in(&fixture.root)
        .unwrap()
        .iter()
        .all(|entry| entry.attached_by.is_none()));
    local.acknowledge_recovery_adoption();
    eventually(|| {
        list_running_sessions_in(&fixture.root)
            .unwrap()
            .iter()
            .any(|entry| {
                entry.session.created_at_unix_ms == running.target.generation.get()
                    && entry.attached_by == Some(festerm_sessiond::AttachKind::Local)
            })
    });
    drop(local);
    eventually(|| {
        list_running_sessions_in(&fixture.root)
            .unwrap()
            .iter()
            .any(|entry| {
                entry.session.created_at_unix_ms == running.target.generation.get()
                    && entry.attached_by.is_none()
            })
    });
    assert!(matches!(
        fixture.control().lookup(&fixture.owner, &running.target.logical, allow),
        Ok(Lookup::Running(target)) if target == running.target
    ));
}

#[test]
fn temporary_takeover_fences_old_disconnect_and_persistent_shells_have_no_lease() {
    let fixture = Fixture::new();
    let creation = fixture.creation(Lifetime::Temporary {
        reconnect_grace_seconds: 60,
    });
    let running = fixture.ensure(&creation);
    let initial = running.lease.unwrap();
    let operations = fixture.state()["operations"].clone();
    let directory = storage::private_directory(&fixture.root, false).unwrap();
    let stem = format!("host-daemon-{}", running.target.generation.get());
    let locked =
        storage::open_private_file(&directory, Path::new(&format!("{stem}.lock")), false).unwrap();
    eventually(|| match fs2::FileExt::try_lock_exclusive(&locked) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
        Err(error) => panic!("lease lock failed: {error}"),
    });
    let lease_name = PathBuf::from(format!("{stem}.json"));
    let before = storage::read_private(&directory, &lease_name, 4096).unwrap();
    // Busy is a nonblocking lock outcome, not a new creation or attachment.
    assert!(matches!(
        fixture.control().attach(
            &fixture.owner,
            &running.target,
            &creation.attachment,
            false,
            2 * 1024 * 1024,
            noop_session_event_notifier(),
            allow,
        ),
        Err(ControlError::Busy)
    ));
    assert_eq!(
        storage::read_private(&directory, &lease_name, 4096).unwrap(),
        before
    );
    let before: serde_json::Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(before["temporary"], serde_json::to_value(&initial).unwrap());
    assert_eq!(before["writer_epoch"], 1);
    assert_eq!(before["connected"], false);
    assert!(before["pending"].is_null());
    assert_eq!(fixture.state()["operations"], operations);
    drop(locked);
    let pending = fixture.attach(&running.target, &creation.attachment, false);
    assert_eq!(pending.authority.target(), &running.target);
    assert_eq!(
        pending.reserved_lease.as_ref().unwrap().epoch.get(),
        initial.epoch.get() + 1
    );
    assert!(matches!(
        fixture
            .control()
            .attachment_outcome(&pending.authority, allow,),
        Err(ControlError::Busy)
    ));
    let (first, first_lease, first_authority) = fixture.adopt_authorized(&running.target, pending);
    let first_lease = first_lease.unwrap();
    assert_eq!(first_lease.epoch.get(), initial.epoch.get() + 1);
    assert_eq!(first_lease.lease, initial.lease);
    assert_eq!(first_lease.attachment, creation.attachment);
    assert_eq!(fixture.state()["operations"], operations);
    let native = list_running_sessions_in(&fixture.root).unwrap();
    assert_eq!(native.len(), 1);
    assert_eq!(native[0].session.name, running.identity.name);
    assert_eq!(native[0].session.pid, running.identity.pid);
    assert_eq!(
        native[0].session.created_at_unix_ms,
        running.target.generation.get()
    );
    assert_eq!(native[0].session.endpoint, running.identity.endpoint);
    let next_attachment = AttachmentId::new();
    assert!(matches!(
        fixture.control().attach(
            &fixture.owner,
            &running.target,
            &next_attachment,
            false,
            2 * 1024 * 1024,
            noop_session_event_notifier(),
            allow,
        ),
        Err(ControlError::Busy)
    ));
    let pending = fixture.attach(&running.target, &next_attachment, true);
    fixture
        .control()
        .maintain_lease(&first_authority, None, allow)
        .unwrap();
    let (second, _, second_authority) = fixture.adopt_authorized(&running.target, pending);
    assert!(matches!(
        fixture
            .control()
            .maintain_lease(&first_authority, Some(DetachKind::Intentional), allow),
        Err(ControlError::Stale)
    ));
    fixture
        .control()
        .maintain_lease(&second_authority, None, allow)
        .unwrap();
    drop(first);
    fixture
        .control()
        .maintain_lease(&second_authority, Some(DetachKind::TransportLost), allow)
        .unwrap();
    assert!(matches!(
        fixture
            .control()
            .maintain_lease(&second_authority, None, allow),
        Err(ControlError::Stale)
    ));
    let receipt = fixture
        .control()
        .maintain_lease(&second_authority, Some(DetachKind::Intentional), allow)
        .unwrap()
        .unwrap();
    assert!(matches!(
        fixture.control().with_detachment_receipt_authorized(
            &receipt,
            |_| Err::<(), _>(ControlError::Denied),
            || -> Result<(), ControlError> { panic!("denied detach publication") }
        ),
        Err(ControlError::Denied)
    ));
    fixture
        .control()
        .with_detachment_receipt_authorized(&receipt, allow, || Ok(()))
        .unwrap();
    assert!(matches!(
        fixture
            .control()
            .with_attachment_authorized(&second_authority, allow, || Ok(())),
        Err(ControlError::Ended)
    ));
    drop(second);
    fixture.wait_absent(&running.target);
    assert!(matches!(
        fixture
            .control()
            .with_detachment_receipt_authorized(&receipt, allow, || Ok(())),
        Err(ControlError::Stale)
    ));

    let persistent = fixture.ensure(&fixture.creation(Lifetime::Persistent {}));
    assert!(persistent.lease.is_none());
    let pending = fixture.attach(&persistent.target, &AttachmentId::new(), false);
    let (active, lease, authority) = fixture.adopt_authorized(&persistent.target, pending);
    assert!(lease.is_none());
    let receipt = fixture
        .control()
        .maintain_lease(&authority, Some(DetachKind::Intentional), allow)
        .unwrap()
        .unwrap();
    fixture
        .control()
        .with_detachment_receipt_authorized(&receipt, allow, || Ok(()))
        .unwrap();
    assert!(matches!(
        fixture
            .control()
            .with_attachment_authorized(&authority, allow, || Ok(())),
        Err(ControlError::Stale)
    ));
    drop(active);
    assert!(matches!(
        fixture
            .control()
            .lookup(&fixture.owner, &persistent.target.logical, allow),
        Ok(Lookup::Running(_))
    ));
}

#[test]
fn daemon_enforces_lost_create_reply_grace_without_bridge_or_supervisor() {
    let fixture = Fixture::new();
    let running = fixture.ensure(&fixture.creation(Lifetime::Temporary {
        reconnect_grace_seconds: 60,
    }));
    let persistent = fixture.ensure(&fixture.creation(Lifetime::Persistent {}));
    // Model an already elapsed portion of the fixed 60s grace; production
    // configuration cannot select a shorter policy or turn cleanup off.
    fixture.edit_lease(&running.target, |lease| {
        lease["deadline_ms"] = ((now_ms() + 1200) as u64).into()
    });
    // A second process may not impersonate this already-owned generation.
    assert!(DaemonWatch::open(
        &fixture.root,
        &running.identity.name,
        running.target.generation.get(),
    )
    .is_err());
    drop(fixture.control());
    thread::sleep(Duration::from_millis(1400));
    fixture.wait_absent(&running.target);
    assert!(matches!(
        fixture
            .control()
            .lookup(&fixture.owner, &persistent.target.logical, allow),
        Ok(Lookup::Running(_))
    ));
}

#[test]
fn current_absence_requires_fresh_create_permission_but_live_resume_does_not() {
    let fixture = Fixture::new();
    let creation = fixture.creation(Lifetime::Persistent {});
    let running = fixture.ensure(&creation);
    let no_creation = |state: &AdmissionState| {
        if state.effect == Effect::Create {
            Err(ControlError::Denied)
        } else {
            Ok(())
        }
    };
    let resumed = fixture
        .control()
        .ensure(
            &fixture.owner,
            &fixture.issued(),
            &creation,
            false,
            &AtomicBool::new(false),
            no_creation,
        )
        .unwrap_or_else(|error| {
            panic!(
                "HOST control stage=current-absence/live-resume actual={}",
                control_result_diagnostic::<()>(&Err(error)),
            )
        });
    assert_eq!(resumed.identity, running.identity);
    fixture
        .control()
        .end(&fixture.owner, &running.target, allow)
        .unwrap();
    fixture.wait_absent(&running.target);
    assert!(matches!(
        fixture.control().ensure(
            &fixture.owner,
            &fixture.issued(),
            &creation,
            false,
            &AtomicBool::new(false),
            no_creation
        ),
        Err(ControlError::Denied)
    ));
    assert!(list_running_sessions_in(&fixture.root).unwrap().is_empty());
}

#[test]
fn failed_real_helper_spawn_is_durable_unknown_not_a_fresh_create() {
    let fixture = Fixture::new();
    let creation = fixture.creation(Lifetime::Persistent {});
    // The repository fixture refuses the helper's `start` argv. It cannot
    // publish a daemon, letting us exercise a real failed subprocess outcome.
    let broken = SessionControl::with_helper(
        &fixture.root,
        Path::new(creation.recipe.shell.as_ref().unwrap()),
    )
    .unwrap();
    let operation = fixture.issued();
    assert!(matches!(
        broken.ensure(
            &fixture.owner,
            &operation,
            &creation,
            false,
            &AtomicBool::new(false),
            allow
        ),
        Err(ControlError::Unknown)
    ));
    for next in [&operation, &fixture.issued()] {
        let result = retry_busy(|| {
            fixture.control().ensure(
                &fixture.owner,
                next,
                &creation,
                false,
                &AtomicBool::new(false),
                allow,
            )
        });
        assert!(
            matches!(result, Err(ControlError::Unknown)),
            "failed-spawn replay actual={}",
            control_result_diagnostic(&result),
        );
    }
    assert_eq!(
        fixture
            .control()
            .lookup(&fixture.owner, &creation.logical, allow)
            .unwrap(),
        Lookup::Unknown
    );
    assert!(list_running_sessions_in(&fixture.root).unwrap().is_empty());
}

#[test]
fn cancelled_or_revoked_pending_takeover_never_commits_or_stops_old_owner() {
    use festerm_sessiond::host_control::AttachmentCancellation;
    let fixture = Fixture::new();
    let creation = fixture.creation(Lifetime::Temporary {
        reconnect_grace_seconds: 60,
    });
    let running = fixture.ensure(&creation);
    let pending = fixture.attach(&running.target, &creation.attachment, false);
    let authority = pending.authority.clone();
    assert_eq!(
        fixture
            .control()
            .cancel_attachment(&authority, allow)
            .unwrap(),
        AttachmentCancellation::CancelledBeforeCommit
    );
    pending.session.acknowledge_recovery_adoption();
    drop(pending);
    // The daemon loop rejects the cancelled ticket without taking the writer.
    thread::sleep(Duration::from_millis(100));
    let pending = fixture.attach(&running.target, &creation.attachment, false);
    let authority = pending.authority.clone();
    let (active, lease) = fixture.adopt(&running.target, pending);
    let lease = lease.unwrap();
    assert_eq!(
        fixture
            .control()
            .cancel_attachment(&authority, allow)
            .unwrap(),
        AttachmentCancellation::Committed(Some(lease.clone()))
    );
    let pending = fixture.attach(&running.target, &AttachmentId::new(), true);
    fixture
        .control()
        .revoke_local(&fixture.owner, &creation.logical)
        .unwrap();
    pending.session.acknowledge_recovery_adoption();
    drop(pending);
    thread::sleep(Duration::from_millis(100));
    assert_eq!(list_running_sessions_in(&fixture.root).unwrap().len(), 1);
    // Revocation fences pending ownership, not the persistent process policy.
    let directory = storage::private_directory(&fixture.root, false).unwrap();
    let name = PathBuf::from(format!(
        "host-daemon-{}.json",
        running.target.generation.get()
    ));
    let bytes = storage::read_private(&directory, &name, 4096).unwrap();
    let state: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        state["temporary"]["epoch"].as_u64(),
        Some(lease.epoch.get())
    );
    assert!(state["remote_revoked"].as_bool().unwrap());
    drop(active);
}

#[test]
fn protected_inventory_filters_before_labels_without_frontend_admission() {
    let fixture = Fixture::new();
    let mut creation = fixture.creation(Lifetime::Persistent {});
    creation.name = Some("owned label".into());
    let running = fixture.ensure(&creation);
    let other_owner = PrincipalId::new();
    let mut other = fixture.creation(Lifetime::Persistent {});
    other.name = Some("private other owner".into());
    let other_operation = fixture.issued();
    retry_busy(|| {
        fixture.control().ensure(
            &other_owner,
            &other_operation,
            &other,
            false,
            &AtomicBool::new(false),
            allow,
        )
    })
    .unwrap();
    let inventory = retry_busy(|| fixture.control().inventory(&fixture.owner, allow))
        .unwrap_or_else(|error| {
            panic!(
                "HOST control stage=protected-inventory/initial-owner-inventory actual={}",
                control_result_diagnostic::<()>(&Err(error)),
            )
        });
    assert!(inventory.unknown.is_empty());
    assert_eq!(inventory.sessions.len(), 1);
    assert_eq!(inventory.sessions[0].summary.name, "owned label");
    assert_eq!(inventory.sessions[0].summary.target, running.target);
    assert!(!inventory.sessions[0].summary.attached);
    assert_eq!(inventory.sessions[0].grant.owner, fixture.owner);
    let inspected = retry_busy(|| {
        fixture
            .control()
            .inspect(&fixture.owner, &creation.logical, allow)
    })
    .unwrap();
    assert_eq!(inspected.summary.unwrap(), running.summary);
    assert_eq!(
        inspected.grant.unwrap().revision,
        inventory.sessions[0].grant.revision
    );
    assert!(matches!(
        fixture
            .control()
            .inspect(&fixture.owner, &other.logical, allow),
        Err(ControlError::Denied)
    ));
    assert!(list_running_sessions_in(&fixture.root)
        .unwrap()
        .iter()
        .all(|session| session.attached_by.is_none()));
    retry_busy(|| {
        fixture
            .control()
            .revoke_local(&fixture.owner, &creation.logical)
    })
    .unwrap();
    assert!(
        retry_busy(|| fixture.control().inventory(&fixture.owner, allow))
            .unwrap()
            .sessions
            .is_empty()
    );
    assert_eq!(
        retry_busy(|| fixture.control().inventory(&other_owner, allow))
            .unwrap()
            .sessions
            .len(),
        1
    );
}

#[test]
fn guarded_relay_actions_distinguish_pending_from_committed_and_fence_revocation() {
    let fixture = Fixture::new();
    let creation = fixture.creation(Lifetime::Temporary {
        reconnect_grace_seconds: 60,
    });
    let running = fixture.ensure(&creation);
    let control = fixture.control();
    let pending = fixture.attach(&running.target, &creation.attachment, false);
    let authority = pending.authority.clone();
    assert!(matches!(
        control.with_attachment_authorized(&authority, allow, || Ok(())),
        Err(ControlError::Stale)
    ));
    struct Guard(Arc<AtomicBool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let held = Arc::new(AtomicBool::new(false));
    control
        .with_pending_attachment_authorized(
            &authority,
            |_| {
                held.store(true, Ordering::Release);
                Ok(Guard(held.clone()))
            },
            || {
                assert!(held.load(Ordering::Acquire));
                // Per-generation admission never holds the global journal.
                let directory = storage::private_directory(&fixture.root, false).unwrap();
                let lock =
                    storage::open_private_file(&directory, Path::new("host-sessions.lock"), false)
                        .unwrap();
                fs2::FileExt::try_lock_exclusive(&lock).unwrap();
                Ok(())
            },
        )
        .unwrap();
    assert!(!held.load(Ordering::Acquire));
    let (active, _) = fixture.adopt(&running.target, pending);
    control
        .with_attachment_authorized(&authority, allow, || Ok(()))
        .unwrap();
    assert!(matches!(
        control.with_attachment_authorized(
            &authority,
            |_| Err::<(), _>(ControlError::Denied),
            || -> Result<(), ControlError> { panic!("denied action ran") }
        ),
        Err(ControlError::Denied)
    ));
    control
        .revoke_local(&fixture.owner, &creation.logical)
        .unwrap();
    assert!(matches!(
        control.with_attachment_authorized(&authority, allow, || -> Result<(), ControlError> {
            panic!("revoked action ran")
        }),
        Err(ControlError::Revoked)
    ));
    drop(active);
}

#[test]
fn scoped_admission_guard_covers_reservation_not_spawn_and_late_denial_is_committed() {
    struct Guard {
        root: PathBuf,
        effect: Effect,
        released: Arc<AtomicBool>,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            if self.effect == Effect::Create {
                let directory = storage::private_directory(&self.root, false).unwrap();
                let bytes = storage::read_private(
                    &directory,
                    Path::new("host-sessions.json"),
                    4 * 1024 * 1024,
                )
                .unwrap();
                let state: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                let record = state["sessions"]
                    .as_object()
                    .unwrap()
                    .values()
                    .next()
                    .unwrap();
                assert_eq!(record["phase"], "Launching");
                let generation = record["generation"].as_str().unwrap();
                storage::read_private(
                    &directory,
                    Path::new(&format!("host-daemon-{generation}.json")),
                    4096,
                )
                .unwrap();
                assert!(list_running_sessions_in(&self.root).unwrap().is_empty());
                self.released.store(true, Ordering::Release);
            }
        }
    }
    let fixture = Fixture::new();
    let creation = fixture.creation(Lifetime::Persistent {});
    let released = Arc::new(AtomicBool::new(false));
    let issued = fixture.issued();
    let outcome = fixture.control().ensure(
        &fixture.owner,
        &issued,
        &creation,
        false,
        &AtomicBool::new(false),
        |state| {
            assert_eq!(state.issuance_epoch, Some(issued.epoch));
            if let (Effect::Resume, Some(grant)) = (state.effect, &state.grant) {
                assert!(released.load(Ordering::Acquire));
                assert!(matches!(
                    grant.state,
                    festerm_host_protocol::LogicalState::Running(_)
                ));
                assert!(state.grant_epoch.unwrap().get() > 1);
                return Err(ControlError::Denied);
            }
            Ok(Guard {
                root: fixture.root.clone(),
                effect: state.effect,
                released: released.clone(),
            })
        },
    );
    assert!(matches!(
        outcome,
        Err(ControlError::CommittedButNotPublished)
    ));
    assert_eq!(list_running_sessions_in(&fixture.root).unwrap().len(), 1);
}
