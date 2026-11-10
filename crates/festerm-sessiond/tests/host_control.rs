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
}

impl Drop for Fixture {
    fn drop(&mut self) {
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
                Ok(sessions) if sessions.is_empty() => {
                    let _ = fs::remove_dir_all(&self.root);
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
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
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
    let pending = fixture
        .control()
        .attach(
            &fixture.owner,
            &running.target,
            &AttachmentId::new(),
            true,
            2 * 1024 * 1024,
            noop_session_event_notifier(),
            allow,
        )
        .unwrap();
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
