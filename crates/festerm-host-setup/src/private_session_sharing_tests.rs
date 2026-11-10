use super::*;
use crate::{
    enrollment::{
        owner_key_fingerprint,
        tests::{fixture_owners, setup_origin},
    },
    lifecycle::Origin,
};
use festerm_host_protocol::Capabilities;
#[cfg(unix)]
use festerm_sessiond::native_binding::BindingPhase;
use festerm_tunnel_bridge::owner_auth::OwnerSigningKey;
use std::sync::atomic::Ordering;

fn approve(owners: &OwnerEnrollment<'_>) -> (crate::enrollment::EnrollmentSnapshot, [u8; 32]) {
    let initial = owners
        .initialize(Path::new("host-identity"), "github:1234".into())
        .unwrap();
    let key = OwnerSigningKey::from_seed(&[47; 32]).public_key();
    let enrolled = owners
        .enroll(initial.revision, key, &owner_key_fingerprint(&key).unwrap())
        .unwrap();
    (
        owners
            .approve_permissions(
                enrolled.revision,
                true,
                Capabilities {
                    inventory: true,
                    attach: true,
                    detach: true,
                    ..Default::default()
                },
            )
            .unwrap(),
        key,
    )
}

#[test]
fn private_sharing_requires_current_desktop_native_vault_not_portable_or_headless_authority() {
    let (root, store, vault) = setup_origin(Origin::Desktop);
    let state = root.path().join("installation");
    let owners = fixture_owners(&store, vault.clone());
    let (approved, _) = approve(&owners);
    let sharing = Sharing::open(&store, &state, owners).unwrap();
    assert!(
        !state.join("daemon").exists(),
        "opening sharing must remain read-only"
    );
    let mut consent = Consent {
        expected: store.snapshot().unwrap(),
        owner_revision: approved.revision,
        operation: OperationId::new(),
    };
    assert!(sharing.consent(&consent).is_ok());
    assert!(sharing.lookup(&consent.operation).unwrap().is_none());
    assert!(
        !state.join("daemon").exists(),
        "lookup cannot initialize a namespace"
    );
    #[cfg(unix)]
    {
        let retained = root.path().join("retained-installation");
        std::fs::rename(&state, &retained).unwrap();
        storage::private_directory(&state, true).unwrap();
        assert_eq!(sharing.lookup(&consent.operation), Err(Error::Denied));
        assert!(matches!(sharing.consent(&consent), Err(Error::Denied)));
        assert!(!state.join("daemon").exists());
        std::fs::remove_dir(&state).unwrap();
        std::fs::rename(&retained, &state).unwrap();
        assert!(sharing.consent(&consent).is_ok());
    }
    consent.owner_revision = consent.owner_revision.next().unwrap();
    assert!(matches!(sharing.consent(&consent), Err(Error::Stale)));
    consent.owner_revision = approved.revision;
    consent.expected.binding.as_mut().unwrap().account = "github:9999".into();
    assert!(matches!(sharing.consent(&consent), Err(Error::Stale)));
    consent.expected = store.snapshot().unwrap();
    vault.locked.store(true, Ordering::Release);
    assert_eq!(sharing.lookup(&consent.operation), Err(Error::Denied));
    vault.locked.store(false, Ordering::Release);
    let foreign = storage::private_test_directory();
    assert!(matches!(
        Sharing::open(
            &store,
            foreign.path(),
            fixture_owners(&store, vault.clone())
        ),
        Err(Error::Denied)
    ));
    let (headless_root, headless, headless_vault) = setup_origin(Origin::Headless);
    assert!(matches!(
        Sharing::open(
            &headless,
            &headless_root.path().join("installation"),
            fixture_owners(&headless, headless_vault)
        ),
        Err(Error::Denied)
    ));
    store.revoke().unwrap();
    assert!(matches!(sharing.consent(&consent), Err(Error::Stale)));
}

#[test]
fn private_sharing_requires_separate_enrolled_key_and_narrow_explicit_access() {
    let (root, store, vault) = setup_origin(Origin::Desktop);
    let owners = fixture_owners(&store, vault);
    let initial = owners
        .initialize(Path::new("host-identity"), "github:1234".into())
        .unwrap();
    let sharing = Sharing::open(&store, &root.path().join("installation"), owners).unwrap();
    let consent = Consent {
        expected: store.snapshot().unwrap(),
        owner_revision: initial.revision,
        operation: OperationId::new(),
    };
    assert!(matches!(sharing.consent(&consent), Err(Error::Revoked)));
    let bytes = storage::read_private(
        &storage::private_directory(&root.path().join("installation"), false).unwrap(),
        Path::new("owner-enrollment.json"),
        16384,
    )
    .unwrap();
    let original = bytes.to_vec();
    let directory = storage::private_directory(&root.path().join("installation"), false).unwrap();
    storage::replace_private(&directory, Path::new("owner-enrollment.json"), b"{}").unwrap();
    assert_eq!(sharing.lookup(&consent.operation), Err(Error::Denied));
    storage::replace_private(&directory, Path::new("owner-enrollment.json"), &original).unwrap();
    assert!(sharing.lookup(&consent.operation).unwrap().is_none());
}

#[cfg(unix)]
#[test]
#[ignore = "one explicit current-source helper and test-owned ordinary Local frontend; no installed runtime/provider/OS services"]
fn private_sharing_native_ordinary_local_bind_switch_resume_revoke_release_preserves_frontend() {
    use festerm_session::{noop_session_event_notifier, Session, SessionEvent};
    use festerm_sessiond::{AttachKind, PersistentSession};
    use sha2::{Digest, Sha256};
    use std::{
        fs::File,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    fn run(command: &mut Command) {
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("test-owned helper command exceeded bounded deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn marker(frontend: &PersistentSession, value: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        while frontend.take_recovered_terminal().is_none() {
            if frontend.local_session_target().is_none() {
                panic!("original local provenance lost");
            }
            // Subsequent calls have already adopted the initial recovery.
            if frontend.lifecycle() == festerm_session::SessionLifecycle::Running {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        frontend
            .try_send_input(format!("{value}\n").as_bytes())
            .unwrap();
        let expected = format!("PRIVATE-PTY:{value}");
        loop {
            if let Ok(SessionEvent::Output(bytes)) = frontend.try_recv_event() {
                output.extend(bytes);
            }
            if output
                .windows(expected.len())
                .any(|bytes| bytes == expected.as_bytes())
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "original PTY did not echo fixture marker"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    struct NativeCleanup {
        helper: std::path::PathBuf,
        runtime: std::path::PathBuf,
        selected: festerm_sessiond::UnattachedSession,
        log: File,
    }
    impl Drop for NativeCleanup {
        fn drop(&mut self) {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if festerm_sessiond::list_running_sessions_strict_in(&self.runtime)
                    .is_ok_and(|values| values.is_empty())
                {
                    return;
                }
                match festerm_sessiond::list_unattached_sessions_in(&self.runtime) {
                    Ok(values) if values.iter().any(|value| value == &self.selected) => {
                        let exact = festerm_sessiond::terminate_unattached_session_command_in(
                            &self.selected,
                            &self.runtime,
                        )
                        .unwrap();
                        let mut command = Command::new(&self.helper);
                        command
                            .args(exact.get_args())
                            .env("FESTERM_SESSIOND_RUNTIME_ROOT", &self.runtime)
                            .stdout(self.log.try_clone().unwrap())
                            .stderr(self.log.try_clone().unwrap());
                        run(&mut command);
                        return;
                    }
                    _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                    _ => panic!("test-owned exact native cleanup remains uncertain"),
                }
            }
        }
    }

    let helper = std::path::PathBuf::from(
        std::env::var_os("FESTERM_PRIVATE_SHARING_HELPER").expect("explicit fresh source helper"),
    );
    assert!(helper.is_absolute() && helper.is_file());
    let project = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let native_path = project.join(format!(
        "p{}",
        &uuid::Uuid::new_v4().simple().to_string()[..6]
    ));
    assert!(!native_path.exists());
    let native = storage::private_directory(&native_path, true).unwrap();
    let runtime = native_path.join("r");
    storage::private_directory(&runtime, true).unwrap();
    let log = storage::open_private_file(&native, Path::new("native.log"), true).unwrap();
    let name = "ordinary-private-local";
    let mut start = Command::new(&helper);
    start.env("FESTERM_SESSIOND_RUNTIME_ROOT", &runtime)
        .env_remove("FESTERM_SESSIOND_HOST_CONTROL").env_remove("FESTERM_SESSIOND_GENERATION")
        .env_remove("FESTERM_SESSIOND_LAUNCH_REQUEST").env_remove("FESTERM_SESSIOND_EXPECTED_SPAWN")
        .args(["start", "--name", name, "--shell", "/bin/sh", "--arg", "-c", "--arg",
            "while IFS= read -r line; do if [ \"$line\" = 'PRIVATE-EXIT' ]; then exit 0; fi; printf 'PRIVATE-PTY:%s\\n' \"$line\"; done", "--cwd"])
        .arg(&native_path).stdin(Stdio::null()).stdout(log.try_clone().unwrap()).stderr(log.try_clone().unwrap());
    run(&mut start);
    let selected = festerm_sessiond::list_unattached_sessions_in(&runtime)
        .unwrap()
        .pop()
        .unwrap();
    let cleanup = NativeCleanup {
        helper,
        runtime: runtime.clone(),
        selected: selected.clone(),
        log,
    };
    let frontend =
        PersistentSession::resume_discovered_in(&selected, &runtime, noop_session_event_notifier())
            .unwrap();
    marker(&frontend, "before-bind");
    let target = frontend.local_session_target().unwrap();
    assert_eq!(target.runtime_root(), runtime);
    assert_eq!(target.identity().pid, selected.pid);
    assert_eq!(
        target.identity().created_at_unix_ms,
        selected.created_at_unix_ms
    );
    let before = festerm_sessiond::list_running_sessions_strict_in(&runtime).unwrap();
    assert_eq!(before[0].attached_by, Some(AttachKind::Local));
    for name in [
        "installation.json",
        "profile-consent.json",
        "script-ownership.json",
        "host-identity.key",
        "host-migration-claim.json",
    ] {
        assert!(!runtime.join(name).exists());
    }
    let capability = format!(
        "native-binding-{}-{}.key",
        selected.pid, selected.created_at_unix_ms
    );
    let native_directory = storage::private_directory(&runtime, false).unwrap();
    let digest = || {
        Sha256::digest(
            &*storage::read_private(&native_directory, Path::new(&capability), 4096).unwrap(),
        )
    };
    let key_before = digest();

    let (authority_root, store, vault) = setup_origin(Origin::Desktop);
    let owners = fixture_owners(&store, vault.clone());
    let (approved, key) = approve(&owners);
    let state = authority_root.path().join("installation");
    let sharing = Sharing::open(&store, &state, owners).unwrap();
    let original_snapshot = store.snapshot().unwrap();
    let consent = Consent {
        expected: original_snapshot.clone(),
        owner_revision: approved.revision,
        operation: OperationId::new(),
    };
    assert_eq!(
        sharing.bind(&consent, &target, || false),
        Err(Error::Cancelled)
    );
    assert!(sharing.lookup(&consent.operation).unwrap().is_none());
    let prepared = sharing.bind(&consent, &target, || true).unwrap();
    assert_eq!(prepared.phase, BindingPhase::Prepared);
    assert!(!prepared.binding.source_authority.is_legacy());
    assert_eq!(sharing.bind(&consent, &target, || true).unwrap(), prepared);
    let mut calls = 0;
    assert_eq!(
        sharing.bind(&consent, &target, || {
            calls += 1;
            calls < 4
        }),
        Err(Error::Cancelled)
    );
    assert_eq!(
        sharing.lookup(&consent.operation).unwrap().unwrap().binding,
        prepared.binding
    );
    marker(&frontend, "after-private-bind");
    let mut forged = prepared.clone();
    forged.binding.identity.created_at_unix_ms += 1;
    assert_eq!(sharing.switch(&forged, || true), Err(Error::Denied));
    assert_eq!(sharing.switch(&prepared, || false), Err(Error::Cancelled));
    let active_receipt = sharing.switch(&prepared, || true).unwrap();
    assert_eq!(active_receipt.phase, BindingPhase::Active);
    marker(&frontend, "after-switch");
    drop(sharing);
    let sharing = Sharing::open(&store, &state, fixture_owners(&store, vault)).unwrap();
    let resumed = sharing.lookup(&consent.operation).unwrap().unwrap();
    assert_eq!(resumed.binding, prepared.binding);
    assert_eq!(
        sharing.bind(&consent, &target, || true).unwrap().phase,
        BindingPhase::Active
    );
    marker(&frontend, "after-resume");
    sharing.owners.revoke(approved.revision, key).unwrap();
    assert!(matches!(
        sharing.switch(&resumed, || true),
        Err(Error::Revoked | Error::Stale)
    ));
    let released = sharing
        .release(&sharing.lookup(&consent.operation).unwrap().unwrap())
        .unwrap();
    assert_eq!(released.phase, BindingPhase::Released);
    assert_eq!(sharing.rollback(&released).unwrap(), released);
    assert!(sharing.switch(&released, || true).is_err());
    marker(&frontend, "after-revocation-release-rollback");
    assert_eq!(key_before, digest());
    assert_eq!(
        before,
        festerm_sessiond::list_running_sessions_strict_in(&runtime).unwrap()
    );
    assert_eq!(store.snapshot().unwrap(), original_snapshot);
    assert_eq!(frontend.local_session_target().unwrap(), target);
    assert!(prepared.binding.source_installation.is_empty());
    assert!(prepared.binding.source_installation_digest.is_empty());
    frontend.try_send_input(b"PRIVATE-EXIT\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !festerm_sessiond::list_running_sessions_strict_in(&runtime)
        .unwrap()
        .is_empty()
    {
        let _ = frontend.try_recv_event();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !runtime.join(&capability).exists(),
        "ordinary exit retained generation capability"
    );
    storage::write_new_private(&native, Path::new("sharing-evidence.json"), &serde_json::to_vec(
        &serde_json::json!({
            "identity": target.identity(),
            "original_frontend_kind": before[0].attached_by,
            "original_frontend_registry_unchanged": true,
            "prepared": prepared,
            "active": active_receipt,
            "released": released,
            "original_capability_sha256": key_before.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
            "master_snapshot_unchanged": original_snapshot,
            "markers": ["before-bind", "after-private-bind", "after-switch", "after-resume", "after-revocation-release-rollback"],
            "ordinary_exit_key_absent": true,
            "legacy_source_files_created": false,
        })
    ).unwrap()).unwrap();
    println!("ordinary-private Local case: same PID/generation/frontend/PTY through bind/switch/resume/revoke/release/rollback; no legacy source receipts, provider, frontend acquisition by sharing or duplicate shell; fixture={}", native_path.display());
    drop(frontend);
    drop(cleanup);
    if let Some(evidence) = std::env::var_os("FESTERM_PRIVATE_SHARING_EVIDENCE") {
        let destination = std::path::PathBuf::from(evidence);
        assert!(destination.is_absolute() && destination.is_dir());
        std::fs::rename(
            &native_path,
            destination.join(native_path.file_name().unwrap()),
        )
        .unwrap();
    } else {
        std::fs::remove_dir_all(&native_path).unwrap();
    }
}
