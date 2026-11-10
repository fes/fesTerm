#![cfg(unix)]

use festerm_host_protocol::{
    AttachmentId, Creation, DetachKind, GenerationId, Lifetime, LogicalSessionId, OperationId,
    PrincipalId, Recipe, Target,
};
use festerm_session::noop_session_event_notifier;
use festerm_sessiond::{
    host_control::{
        AdmissionState, ControlError, Effect, Lookup, PendingAttachment, SessionControl,
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
        Self {
            root,
            owner: PrincipalId::new(),
        }
    }

    fn control(&self) -> SessionControl {
        SessionControl::with_helper(
            &self.root,
            Path::new(env!("CARGO_BIN_EXE_festerm-sessiond")),
        )
        .unwrap()
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
    let operation = OperationId::new();
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
    let launch = fixture.root.join(format!("launch-{operation}.json"));
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
    assert!(control.inventory(&fixture.owner, allow).unwrap().is_empty());
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
    assert!(control.inventory(&fixture.owner, allow).unwrap().is_empty());
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
fn operation_capacity_retires_receipts_without_resurrection_or_resume_outage() {
    use festerm_host_protocol::MAX_IDEMPOTENCY_RECORDS;
    let fixture = Fixture::new();
    let creation = fixture.creation(Lifetime::Persistent {});
    let running = fixture.ensure(&creation);
    let mut state = fixture.state();
    let ids: Vec<_> = (0..MAX_IDEMPOTENCY_RECORDS)
        .map(|_| OperationId::new())
        .collect();
    for id in &ids {
        state["operations"][id.as_str()] = serde_json::json!({
            "owner": fixture.owner,
            "creation": creation,
            "generation": running.target.generation,
            "created": false,
            "recreated": false,
            "create_only": false,
        });
    }
    fixture.save_state(&state);
    for _ in 0..MAX_IDEMPOTENCY_RECORDS + 2 {
        assert_eq!(fixture.ensure(&creation).target, running.target);
    }
    assert!(fixture.state()["operations"]
        .as_object()
        .unwrap()
        .is_empty());
    assert_eq!(
        fixture
            .control()
            .ensure(
                &fixture.owner,
                &ids[0],
                &creation,
                false,
                &AtomicBool::new(false),
                allow
            )
            .unwrap()
            .target,
        running.target,
    );
    fixture
        .control()
        .end(&fixture.owner, &running.target, allow)
        .unwrap();
    fixture.wait_absent(&running.target);
    let replacement = fixture.ensure(&creation);
    assert_ne!(replacement.target.generation, running.target.generation);
    assert!(matches!(
        fixture.control().ensure(
            &fixture.owner,
            &ids[0],
            &creation,
            false,
            &AtomicBool::new(false),
            allow
        ),
        Err(ControlError::Ended)
    ));
    let mut changed = creation.clone();
    changed.recipe.args.push("changed".into());
    assert!(matches!(
        fixture.control().ensure(
            &fixture.owner,
            &ids[0],
            &changed,
            false,
            &AtomicBool::new(false),
            allow
        ),
        Err(ControlError::OperationConflict)
    ));
}

#[test]
fn logical_capacity_retires_absent_records_but_never_erases_revocation_or_generation_fences() {
    use festerm_host_protocol::MAX_SESSIONS;
    use sha2::{Digest, Sha256};
    let fixture = Fixture::new();
    let creation = fixture.creation(Lifetime::Persistent {});
    let running = fixture.ensure(&creation);
    fixture
        .control()
        .end(&fixture.owner, &running.target, allow)
        .unwrap();
    fixture.wait_absent(&running.target);
    let mut state = fixture.state();
    let original = state["sessions"][creation.logical.as_str()].clone();
    let mut revoked = None;
    for index in 1..MAX_SESSIONS {
        let logical = LogicalSessionId::new();
        let mut record = original.clone();
        record["logical"] = logical.as_str().into();
        let mut hash = Sha256::new();
        hash.update(b"festerm-host-session-v1\0");
        hash.update(fixture.owner.as_str());
        hash.update(logical.as_str());
        record["name"] = format!("hs-{:x}", hash.finalize())[..51].into();
        record["revoked"] = (index == 1).into();
        if index == 1 {
            revoked = Some(logical.clone());
        }
        state["sessions"][logical.as_str()] = record;
    }
    fixture.save_state(&state);
    let next = fixture.creation(Lifetime::Persistent {});
    let replacement = fixture
        .control()
        .ensure(
            &fixture.owner,
            &OperationId::new(),
            &next,
            false,
            &AtomicBool::new(false),
            allow,
        )
        .unwrap_or_else(|error| {
            panic!(
                "after logical capacity retirement: {error}; active journal: {}",
                fixture.state()
            )
        });
    assert!(replacement.target.generation > running.target.generation);
    assert_eq!(fixture.state()["sessions"].as_object().unwrap().len(), 1);
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
    assert!(matches!(
        fixture.control().ensure(
            &fixture.owner,
            &OperationId::new(),
            &creation,
            false,
            &AtomicBool::new(false),
            allow
        ),
        Err(ControlError::Ended)
    ));
}

#[test]
fn local_quarantine_retirement_releases_pending_budget_without_asserting_absence() {
    use festerm_host_protocol::MAX_PENDING_CREATES;
    let fixture = Fixture::new();
    let (control, marker) = fixture.failing_control();
    let mut pending = Vec::new();
    for _ in 0..MAX_PENDING_CREATES {
        let creation = fixture.creation(Lifetime::Persistent {});
        let operation = OperationId::new();
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
            &OperationId::new(),
            &next,
            false,
            &AtomicBool::new(false),
            allow
        ),
        Err(ControlError::Busy)
    ));
    fs::remove_file(&marker).unwrap();
    let interrupted = fixture.state();
    for (creation, _, generation) in &pending {
        let target = Target {
            logical: creation.logical.clone(),
            generation: *generation,
        };
        assert!(matches!(
            control.retire_local(&PrincipalId::new(), &target),
            Err(ControlError::Denied)
        ));
        control.retire_local(&fixture.owner, &target).unwrap();
        assert!(DaemonWatch::open(
            &fixture.root,
            interrupted["sessions"][creation.logical.as_str()]["name"]
                .as_str()
                .unwrap(),
            generation.get()
        )
        .unwrap()
        .should_stop());
    }
    // Crash after durable tombstones but before active-journal deletion.
    fixture.save_state(&interrupted);
    let running = fixture.ensure(&next);
    assert_eq!(fixture.state()["sessions"].as_object().unwrap().len(), 1);
    for (creation, operation, _) in &pending {
        assert_eq!(
            control
                .lookup(&fixture.owner, &creation.logical, allow)
                .unwrap(),
            Lookup::Unknown
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
            Err(ControlError::Unknown)
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

impl Fixture {
    fn ensure(&self, creation: &Creation) -> festerm_sessiond::host_control::EnsureResult {
        self.control()
            .ensure(
                &self.owner,
                &OperationId::new(),
                creation,
                false,
                &AtomicBool::new(false),
                allow,
            )
            .unwrap()
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
        target: &Target,
        pending: PendingAttachment,
    ) -> (
        festerm_sessiond::PersistentSession,
        Option<festerm_host_protocol::TemporaryLease>,
    ) {
        eventually(|| pending.session.take_recovered_terminal_deferred().is_some());
        pending.session.acknowledge_recovery_adoption();
        if let (Some(ticket), Some(expected)) = (&pending.ticket, &pending.reserved_lease) {
            eventually(|| {
                match self.control().attachment_outcome(
                    &self.owner,
                    target,
                    ticket,
                    expected,
                    allow,
                ) {
                    Ok(_) => true,
                    Err(ControlError::Busy | ControlError::Storage) => false,
                    Err(error) => panic!("attachment commit failed: {error}"),
                }
            });
        }
        (pending.session, pending.reserved_lease)
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
    let operation = Arc::new(OperationId::new());
    let barrier = Arc::new(Barrier::new(6));
    let workers: Vec<_> = (0..6)
        .map(|index| {
            let control = fixture.control();
            let owner = fixture.owner.clone();
            let creation = creation.clone();
            let operation = if index % 2 == 0 {
                Arc::new(OperationId::new())
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
    let replay = fixture
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
    assert_eq!(replay.identity, results[0].identity);
    let mut changed = (*creation).clone();
    changed.recipe.args.push("emit:not-run".into());
    assert!(matches!(
        fixture.control().ensure(
            &fixture.owner,
            &OperationId::new(),
            &changed,
            false,
            &AtomicBool::new(false),
            allow
        ),
        Err(ControlError::RecipeConflict)
    ));
    assert!(matches!(
        fixture.control().ensure(
            &fixture.owner,
            &operation,
            &changed,
            false,
            &AtomicBool::new(false),
            allow
        ),
        Err(ControlError::OperationConflict)
    ));
}

#[test]
fn crash_before_spawn_stays_unknown_and_after_spawn_adopts_only_reserved_generation() {
    let fixture = Fixture::new();
    let creation = fixture.creation(Lifetime::Persistent {});
    let operation = OperationId::new();
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
    let launch = fixture.root.join(format!("launch-{operation}.json"));
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
    assert!(matches!(
        fixture.control().ensure(
            &fixture.owner,
            &OperationId::new(),
            &creation,
            false,
            &AtomicBool::new(false),
            |_| Err::<(), _>(ControlError::Denied)
        ),
        Err(ControlError::Denied)
    ));
    let cancelled = AtomicBool::new(true);
    assert!(matches!(
        fixture.control().ensure(
            &fixture.owner,
            &OperationId::new(),
            &creation,
            false,
            &cancelled,
            allow
        ),
        Err(ControlError::Cancelled)
    ));
    cancelled.store(false, Ordering::Release);
    let running = fixture
        .control()
        .ensure(
            &fixture.owner,
            &OperationId::new(),
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
            &OperationId::new(),
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
fn temporary_takeover_fences_old_disconnect_and_persistent_shells_have_no_lease() {
    let fixture = Fixture::new();
    let creation = fixture.creation(Lifetime::Temporary {
        reconnect_grace_seconds: 60,
    });
    let running = fixture.ensure(&creation);
    let initial = running.lease.unwrap();
    let pending = fixture
        .control()
        .attach(
            &fixture.owner,
            &running.target,
            &creation.attachment,
            false,
            2 * 1024 * 1024,
            noop_session_event_notifier(),
            allow,
        )
        .unwrap();
    assert!(matches!(
        fixture.control().attachment_outcome(
            &fixture.owner,
            &running.target,
            pending.ticket.as_ref().unwrap(),
            pending.reserved_lease.as_ref().unwrap(),
            allow,
        ),
        Err(ControlError::Busy)
    ));
    let (first, first_lease) = fixture.adopt(&running.target, pending);
    let first_lease = first_lease.unwrap();
    assert_ne!(first_lease.epoch, initial.epoch);
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
    let pending = fixture
        .control()
        .attach(
            &fixture.owner,
            &running.target,
            &next_attachment,
            true,
            2 * 1024 * 1024,
            noop_session_event_notifier(),
            allow,
        )
        .unwrap();
    fixture
        .control()
        .maintain_lease(&fixture.owner, &running.target, &first_lease, None, allow)
        .unwrap();
    let (second, second_lease) = fixture.adopt(&running.target, pending);
    let second_lease = second_lease.unwrap();
    assert_eq!(
        fixture.control().maintain_lease(
            &fixture.owner,
            &running.target,
            &first_lease,
            Some(DetachKind::Intentional),
            allow
        ),
        Err(ControlError::Stale)
    );
    fixture
        .control()
        .maintain_lease(&fixture.owner, &running.target, &second_lease, None, allow)
        .unwrap();
    drop(first);
    fixture
        .control()
        .maintain_lease(
            &fixture.owner,
            &running.target,
            &second_lease,
            Some(DetachKind::TransportLost),
            allow,
        )
        .unwrap();
    assert_eq!(
        fixture.control().maintain_lease(
            &fixture.owner,
            &running.target,
            &second_lease,
            None,
            allow
        ),
        Err(ControlError::Stale)
    );
    fixture
        .control()
        .maintain_lease(
            &fixture.owner,
            &running.target,
            &second_lease,
            Some(DetachKind::Intentional),
            allow,
        )
        .unwrap();
    drop(second);
    fixture.wait_absent(&running.target);

    let persistent = fixture.ensure(&fixture.creation(Lifetime::Persistent {}));
    assert!(persistent.lease.is_none());
    assert_eq!(
        fixture.control().maintain_lease(
            &fixture.owner,
            &persistent.target,
            &second_lease,
            Some(DetachKind::Intentional),
            allow
        ),
        Err(ControlError::Stale)
    );
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
    let mut restarted = DaemonWatch::open(
        &fixture.root,
        &running.identity.name,
        running.target.generation.get(),
    )
    .unwrap();
    drop(fixture.control());
    thread::sleep(Duration::from_millis(1400));
    assert!(restarted.should_stop());
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
            &OperationId::new(),
            &creation,
            false,
            &AtomicBool::new(false),
            no_creation,
        )
        .unwrap();
    assert_eq!(resumed.identity, running.identity);
    fixture
        .control()
        .end(&fixture.owner, &running.target, allow)
        .unwrap();
    fixture.wait_absent(&running.target);
    assert!(matches!(
        fixture.control().ensure(
            &fixture.owner,
            &OperationId::new(),
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
    let operation = OperationId::new();
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
    for next in [&operation, &OperationId::new()] {
        assert!(matches!(
            fixture.control().ensure(
                &fixture.owner,
                next,
                &creation,
                false,
                &AtomicBool::new(false),
                allow
            ),
            Err(ControlError::Unknown)
        ));
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
    let ticket = pending.ticket.clone().unwrap();
    assert_eq!(
        fixture
            .control()
            .cancel_attachment(&fixture.owner, &running.target, &ticket, allow,)
            .unwrap(),
        AttachmentCancellation::CancelledBeforeCommit
    );
    pending.session.acknowledge_recovery_adoption();
    drop(pending);
    // The daemon loop rejects the cancelled ticket without taking the writer.
    thread::sleep(Duration::from_millis(100));
    let pending = fixture.attach(&running.target, &creation.attachment, false);
    let ticket = pending.ticket.clone().unwrap();
    let (active, lease) = fixture.adopt(&running.target, pending);
    let lease = lease.unwrap();
    assert_eq!(
        fixture
            .control()
            .cancel_attachment(&fixture.owner, &running.target, &ticket, allow,)
            .unwrap(),
        AttachmentCancellation::Committed(lease.clone())
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
    fixture
        .control()
        .ensure(
            &other_owner,
            &OperationId::new(),
            &other,
            false,
            &AtomicBool::new(false),
            allow,
        )
        .unwrap();
    let inventory = fixture.control().inventory(&fixture.owner, allow).unwrap();
    assert_eq!(inventory.len(), 1);
    assert_eq!(inventory[0].summary.name, "owned label");
    assert_eq!(inventory[0].summary.target, running.target);
    assert!(!inventory[0].summary.attached);
    assert_eq!(inventory[0].grant.owner, fixture.owner);
    let inspected = fixture
        .control()
        .inspect(&fixture.owner, &creation.logical, allow)
        .unwrap();
    assert_eq!(inspected.summary.unwrap(), running.summary);
    assert_eq!(
        inspected.grant.unwrap().revision,
        inventory[0].grant.revision
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
    fixture
        .control()
        .revoke_local(&fixture.owner, &creation.logical)
        .unwrap();
    assert!(fixture
        .control()
        .inventory(&fixture.owner, allow)
        .unwrap()
        .is_empty());
    assert_eq!(
        fixture
            .control()
            .inventory(&other_owner, allow)
            .unwrap()
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
    let ticket = pending.ticket.as_ref().unwrap();
    let expected = pending.reserved_lease.as_ref().unwrap();
    assert!(matches!(
        control.with_attachment_authorized(
            &fixture.owner,
            &running.target,
            Some(expected),
            allow,
            || Ok(())
        ),
        Err(ControlError::Stale)
    ));
    assert!(matches!(
        control.with_pending_attachment_authorized(
            &fixture.owner,
            &running.target,
            &OperationId::new(),
            expected,
            allow,
            || Ok(())
        ),
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
            &fixture.owner,
            &running.target,
            ticket,
            expected,
            |_| {
                held.store(true, Ordering::Release);
                Ok(Guard(held.clone()))
            },
            || {
                assert!(held.load(Ordering::Acquire));
                assert!(matches!(
                    control.lookup(&fixture.owner, &creation.logical, allow),
                    Err(ControlError::Busy)
                ));
                Ok(())
            },
        )
        .unwrap();
    assert!(!held.load(Ordering::Acquire));
    let (active, committed) = fixture.adopt(&running.target, pending);
    control
        .with_attachment_authorized(
            &fixture.owner,
            &running.target,
            committed.as_ref(),
            allow,
            || Ok(()),
        )
        .unwrap();
    assert!(matches!(
        control.with_attachment_authorized(
            &fixture.owner,
            &running.target,
            committed.as_ref(),
            |_| Err::<(), _>(ControlError::Denied),
            || -> Result<(), ControlError> { panic!("denied action ran") }
        ),
        Err(ControlError::Denied)
    ));
    control
        .revoke_local(&fixture.owner, &creation.logical)
        .unwrap();
    assert!(matches!(
        control.with_attachment_authorized(
            &fixture.owner,
            &running.target,
            committed.as_ref(),
            allow,
            || -> Result<(), ControlError> { panic!("revoked action ran") }
        ),
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
    let outcome = fixture.control().ensure(
        &fixture.owner,
        &OperationId::new(),
        &creation,
        false,
        &AtomicBool::new(false),
        |state| {
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
