use super::*;
use crate::lifecycle::{Origin, Settings, Startup};
use serde_json::json;
use std::path::{Path, PathBuf};

struct Fixture {
    root: PathBuf,
    #[cfg(windows)]
    private_root: Option<storage::PrivateTestDirectory>,
    inventory: Inventory,
    destination: Store,
    selection: Selection,
    source: PathBuf,
    retain: bool,
}

impl Fixture {
    fn new(desktop: bool) -> Self {
        #[cfg(windows)]
        let private_root = storage::private_test_directory();
        #[cfg(windows)]
        let root = private_root.path().to_owned();
        #[cfg(unix)]
        let root = {
            let project = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap();
            project.join(format!("mi-{}", &Uuid::new_v4().simple().to_string()[..8]))
        };
        storage::private_directory(&root, true).unwrap();
        let inventory = Inventory {
            desktop: root.join("profile-hosts"),
            script: root.join(".local").join("share").join("festerm-tunnels"),
            legacy_runtime: root.join("festerm").join("sessiond"),
            native_evidence: Some(|_, _| Ok(true)),
        };
        for path in [
            root.join(".local"),
            root.join(".local").join("share"),
            inventory.script.clone(),
            inventory.desktop.clone(),
            inventory.desktop.join("installations"),
            root.join("festerm"),
            inventory.legacy_runtime.clone(),
        ] {
            storage::private_directory(&path, true).unwrap();
        }
        let installation = Uuid::new_v4().to_string();
        let source = if desktop {
            inventory.desktop.join("installations").join(&installation)
        } else {
            let directory = storage::private_directory(&inventory.script, true).unwrap();
            storage::open_private_file(&directory, Path::new("installer.lock"), true).unwrap();
            inventory.script.join("state")
        };
        let directory = storage::private_directory(&source, true).unwrap();
        storage::open_private_file(&directory, Path::new("installer.lock"), true).unwrap();
        let identity = HostIdentity::load_or_create_from_private_directory(
            &directory,
            Path::new("host-identity.key"),
        )
        .unwrap();
        let consent = Uuid::new_v4().to_string();
        let label = format!("festerm-owned-{}", Uuid::new_v4());
        let tunnel_name = format!(
            "festerm-{}",
            Uuid::parse_str(&installation).unwrap().simple()
        );
        let tunnel = format!("{tunnel_name}.test");
        let runtime = inventory.legacy_runtime.clone();
        let target = profile_hosting::Target {
            runtime_root: runtime.clone(),
            name: if desktop {
                "label-not-identity".into()
            } else {
                tunnel_name.clone()
            },
            pid: 123,
            generation: 456,
            endpoint: runtime.join("123-456.sock").to_string_lossy().into(),
        };
        let origin = desktop.then(|| {
            json!({
                "kind": "desktop-profile", "consent": consent, "spawn_fingerprint": "spawn-proof",
                "ownership_label": label, "incarnation": "original-incarnation",
            })
        });
        let record = json!({
            "version": if desktop { 2 } else { 1 }, "origin": origin, "id": installation,
            "executable": root.join("legacy-helper"), "cli": root.join("legacy-cli"),
            "runtime_root": runtime, "account": "42", "tunnel": tunnel, "phase": "configured",
            "target": { "name": target.name, "pid": target.pid, "generation": target.generation, "endpoint": target.endpoint },
            "fingerprint": identity.fingerprint().unwrap(), "no_takeover": true,
        });
        write(&directory, "installation.json", &record);
        if desktop {
            let profile = profile_hosting::Store::open(&source, &installation, false).unwrap();
            profile
                .prepare(&consent, "spawn-proof", &target.name)
                .unwrap();
            profile.select_account(&consent, "42").unwrap();
            profile.commit_requested(&consent).unwrap();
            profile.bind_target(&consent, target).unwrap();
            let grant = profile.reserve_creation(&consent, "42").unwrap();
            let actual_label = grant
                .cloud_reservation
                .as_ref()
                .unwrap()
                .ownership_label
                .clone();
            let mut record = record;
            record["origin"]["ownership_label"] = json!(actual_label);
            write(&directory, "installation.json", &record);
            profile
                .record_created(
                    &consent,
                    profile_hosting::CloudIdentity {
                        qualified_id: tunnel,
                        incarnation: Some("original-incarnation".into()),
                    },
                )
                .unwrap();
            profile.mark_exposed(&consent).unwrap();
        } else {
            write(
                &directory,
                "script-ownership.json",
                &json!({
                    "version": 1, "installation": installation, "fingerprint": identity.fingerprint().unwrap(),
                    "label": label, "qualified": tunnel, "incarnation": "original-incarnation",
                }),
            );
        }
        let destination = Store::create(
            &root.join("host"),
            Origin::Headless,
            Settings {
                friendly_name: "label-not-identity".into(),
                allow_creation: false,
                startup: Startup::Manual,
            },
        )
        .unwrap();
        Self {
            root,
            #[cfg(windows)]
            private_root: Some(private_root),
            inventory,
            destination,
            source,
            retain: false,
            selection: Selection {
                source: if desktop {
                    Source::DesktopProfile { installation }
                } else {
                    Source::StandaloneScript { installation }
                },
                logical_id: Uuid::new_v4().to_string(),
                label: "label-not-identity".into(),
            },
        }
    }

    fn preview(&self) -> Preview {
        self.inventory
            .preview(
                &self.destination,
                std::slice::from_ref(&self.selection),
                "42",
            )
            .unwrap()
    }

    fn reserve(&self) -> Journal {
        self.inventory
            .reserve(
                &self.destination,
                std::slice::from_ref(&self.selection),
                "42",
                &self.preview().preview_id,
            )
            .unwrap()
    }

    fn source_directory(&self) -> File {
        storage::private_directory(&self.source, false).unwrap()
    }

    fn change(&self, name: &str, action: impl FnOnce(&mut serde_json::Value)) {
        let directory = self.source_directory();
        let mut record: serde_json::Value = read(&directory, name, SOURCE_LIMIT).unwrap();
        action(&mut record);
        write(&directory, name, &record);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let (true, Some(root)) = (self.retain, self.private_root.take()) {
            std::mem::forget(root);
        }
        #[cfg(unix)]
        if !self.retain {
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }
}

#[cfg(unix)]
#[test]
#[ignore = "requires an explicitly supplied current-source helper; no installed runtime or services"]
fn owned_migration_native_reservation_deferral_rollback_preserve_pid_and_existing_frontend() {
    native_migration_fixture(false, None, festerm_sessiond::AttachKind::Local);
}

#[cfg(feature = "owner-enrollment")]
#[test]
fn owned_migration_owner_initialization_and_separate_approval_never_enable_or_widen_grants() {
    use crate::enrollment::OwnerStorage;
    let fixture = Fixture::new(true);
    let reserved = fixture.reserve();
    let owners = fixture
        .destination
        .owner_enrollment_with_storage(OwnerStorage::HeadlessProtectedFile)
        .unwrap();
    assert!(owners
        .initialize_for_migration(&uuid::Uuid::new_v4().to_string(), "42".into())
        .is_err());
    assert!(owners
        .initialize_for_migration(&reserved.operation, "43".into())
        .is_err());
    let initial = owners
        .initialize_for_migration(&reserved.operation, "42".into())
        .unwrap();
    assert!(initial.keys.is_empty());
    assert!(!initial.remote_access);
    assert_eq!(
        initial.capabilities,
        festerm_host_protocol::Capabilities::default()
    );
    let key = ed25519_dalek::SigningKey::from_bytes(&[9; 32])
        .verifying_key()
        .to_bytes();
    assert!(approve_migration_owner(
        &fixture.destination,
        &reserved.operation,
        OwnerStorage::HeadlessProtectedFile,
        key,
        "wrong"
    )
    .is_err());
    let fingerprint = crate::enrollment::owner_key_fingerprint(&key).unwrap();
    let approved = approve_migration_owner(
        &fixture.destination,
        &reserved.operation,
        OwnerStorage::HeadlessProtectedFile,
        key,
        &fingerprint,
    )
    .unwrap();
    assert_eq!(
        approved.capabilities,
        festerm_host_protocol::Capabilities {
            inventory: true,
            attach: true,
            detach: true,
            ..Default::default()
        }
    );
    assert!(
        !owners
            .current_policy()
            .unwrap()
            .verifier()
            .policy()
            .remote_access
    );
    assert!(approve_migration_owner(
        &fixture.destination,
        &reserved.operation,
        OwnerStorage::HeadlessProtectedFile,
        key,
        "wrong"
    )
    .is_err());
    owners.revoke(approved.revision, key).unwrap();
    assert!(owners.migration_policy(&reserved.operation, false).is_err());
    assert!(approve_migration_owner(
        &fixture.destination,
        &reserved.operation,
        OwnerStorage::HeadlessProtectedFile,
        key,
        &fingerprint
    )
    .is_err());
    assert_eq!(
        fixture.destination.snapshot().unwrap().phase,
        HostPhase::Disabled
    );
}

#[cfg(feature = "owner-enrollment")]
#[test]
fn owned_migration_unsupported_helper_defers_without_binding_checkpoint_or_source_mutation() {
    use crate::enrollment::OwnerStorage;
    let fixture = Fixture::new(true);
    let reserved = fixture.reserve();
    let owners = fixture
        .destination
        .owner_enrollment_with_storage(OwnerStorage::HeadlessProtectedFile)
        .unwrap();
    owners
        .initialize_for_migration(&reserved.operation, "42".into())
        .unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&[9; 32])
        .verifying_key()
        .to_bytes();
    let fingerprint = crate::enrollment::owner_key_fingerprint(&key).unwrap();
    approve_migration_owner(
        &fixture.destination,
        &reserved.operation,
        OwnerStorage::HeadlessProtectedFile,
        key,
        &fingerprint,
    )
    .unwrap();
    let original = raw(&fixture.source_directory(), "installation.json").unwrap();
    let target = fixture.preview().mappings[0].target.clone().unwrap();
    let runtime = storage::private_directory(&target.runtime_root, false).unwrap();
    write(
        &runtime,
        "registry.json",
        &json!({"sessions": {
            target.name.clone(): {"name": target.name, "pid": target.pid,
                "created_at_unix_ms": target.generation, "socket": target.endpoint,
                "spawn_fingerprint": format!("v1:{}", "a".repeat(64))}
        }}),
    );
    let error = fixture
        .inventory
        .bind_existing(
            &fixture.destination,
            &fixture.root.join("host"),
            OwnerStorage::HeadlessProtectedFile,
        )
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert!(!has_bindings(&fixture.destination).unwrap());
    assert_eq!(
        original,
        raw(&fixture.source_directory(), "installation.json").unwrap()
    );
    assert_eq!(
        fixture.inventory.defer(&fixture.destination).unwrap().phase,
        Phase::Deferred
    );
    assert_eq!(
        fixture
            .inventory
            .rollback(&fixture.destination)
            .unwrap()
            .destination_host_id,
        reserved.destination_host_id
    );
}

#[cfg(all(unix, feature = "owner-enrollment"))]
#[test]
#[ignore = "explicit current-source binaries and test-owned daemon only; no installed runtime or services"]
fn owned_migration_native_authenticated_binding_switch_resume_release_preserve_frontend() {
    native_migration_fixture(true, None, festerm_sessiond::AttachKind::Remote);
}

#[cfg(all(unix, feature = "owner-enrollment"))]
#[test]
#[ignore = "explicit current-source binaries and test-owned daemon only; no installed runtime or services"]
fn owned_migration_native_authenticated_binding_preserves_local_frontend() {
    native_migration_fixture(true, None, festerm_sessiond::AttachKind::Local);
}

#[cfg(all(unix, feature = "owner-enrollment"))]
#[test]
#[ignore = "explicit current-source binaries and test-owned daemon only; no installed runtime or services"]
fn owned_migration_native_prepared_and_active_shell_exit_release_claims_and_unblock_enable() {
    for active in [false, true] {
        native_migration_fixture(true, Some(active), festerm_sessiond::AttachKind::Remote);
    }
}

#[cfg(unix)]
fn native_migration_fixture(
    _binding: bool,
    _exit_after_switch: Option<bool>,
    frontend: festerm_sessiond::AttachKind,
) {
    use std::{
        io::{Read, Write},
        os::unix::net::UnixStream,
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    fn bounded(command: &mut Command) -> bool {
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status.success();
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                return false;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn input(stream: &mut UnixStream, bytes: &[u8]) {
        stream.write_all(b"FSD1").unwrap();
        stream.write_all(&[1]).unwrap();
        stream
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .unwrap();
        stream.write_all(bytes).unwrap();
        stream.flush().unwrap();
    }

    fn marker(stream: &mut UnixStream, expected: &[u8]) {
        let mut parser = festerm_sessiond::ServerFrameParser::default();
        let mut output = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            while let Some(event) = parser.next_event().unwrap() {
                match event {
                    festerm_sessiond::ServerFrameEvent::Stolen
                    | festerm_sessiond::ServerFrameEvent::Exited => {
                        panic!("original frontend lost")
                    }
                    festerm_sessiond::ServerFrameEvent::Output(bytes) => {
                        output.extend(bytes);
                        assert!(output.len() < 64 * 1024);
                        if output
                            .windows(expected.len())
                            .any(|window| window == expected)
                        {
                            return;
                        }
                    }
                    _ => {}
                }
            }
            let mut bytes = [0; 4096];
            let length = stream.read(&mut bytes).unwrap();
            assert_ne!(length, 0, "original frontend closed");
            parser.push(&bytes[..length]).unwrap();
        }
        panic!("no response on the original frontend");
    }

    #[cfg(feature = "owner-enrollment")]
    fn exit_and_rollback(
        fixture: &Fixture,
        stream: &mut UnixStream,
        selected: &festerm_sessiond::UnattachedSession,
        active: bool,
    ) {
        let runtime = &fixture.inventory.legacy_runtime;
        let journal = binding_status(&fixture.destination).unwrap();
        let binding = journal.bindings.values().next().unwrap();
        assert_eq!(journal.switched.len(), usize::from(active));
        let runtime_directory = storage::private_directory(runtime, false).unwrap();
        let key = format!(
            "native-binding-{}-{}.key",
            selected.pid, selected.created_at_unix_ms
        );
        assert!(runtime.join(&key).exists());
        input(stream, b"MIGRATION-EXIT\n");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if !festerm_sessiond::daemon_generation_is_live(
                runtime,
                selected.pid,
                selected.created_at_unix_ms,
                &selected.endpoint,
            )
            .unwrap()
                && festerm_sessiond::list_running_sessions_strict_in(runtime)
                    .unwrap()
                    .is_empty()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "ordinary shell exit did not finish"
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !runtime.join(&key).exists(),
            "ordinary exit retained generation secret"
        );
        assert!(!runtime
            .join(format!(
                "lease-{}-{}",
                selected.pid, selected.created_at_unix_ms
            ))
            .exists());
        assert!(!Path::new(&selected.endpoint).exists());
        let host_lock = format!("host-daemon-{}.lock", selected.created_at_unix_ms);
        let lock =
            storage::open_private_file(&runtime_directory, Path::new(&host_lock), false).unwrap();
        fs2::FileExt::try_lock_exclusive(&lock).unwrap();
        assert!(fixture
            .inventory
            .release_bindings(&fixture.destination, &fixture.root.join("host"))
            .is_err());
        assert_eq!(
            status(&fixture.destination).unwrap().phase,
            Phase::RollbackPending
        );
        assert!(binding_status(&fixture.destination)
            .unwrap()
            .released
            .is_empty());
        assert!(fixture.inventory.rollback(&fixture.destination).is_err());
        drop(lock);

        let registry =
            storage::read_private(&runtime_directory, Path::new("registry.json"), LIMIT).unwrap();
        storage::replace_private(&runtime_directory, Path::new("registry.json"), b"broken")
            .unwrap();
        assert!(fixture
            .inventory
            .release_bindings(&fixture.destination, &fixture.root.join("host"))
            .is_err());
        assert!(binding_status(&fixture.destination)
            .unwrap()
            .released
            .is_empty());
        assert!(
            !read::<Claim>(&fixture.source_directory(), CLAIM, LIMIT)
                .unwrap()
                .released
        );
        storage::replace_private(&runtime_directory, Path::new("registry.json"), &registry)
            .unwrap();

        if active {
            std::fs::remove_file(runtime.join(&host_lock)).unwrap();
            std::fs::remove_file(
                runtime.join(format!("host-daemon-{}.json", selected.created_at_unix_ms)),
            )
            .unwrap();
        }
        let released = fixture
            .inventory
            .release_bindings(&fixture.destination, &fixture.root.join("host"))
            .unwrap();
        assert_eq!(released.released.len(), 1);
        assert_eq!(
            fixture
                .inventory
                .release_bindings(&fixture.destination, &fixture.root.join("host"))
                .unwrap(),
            released
        );
        assert_eq!(
            fixture
                .inventory
                .rollback(&fixture.destination)
                .unwrap()
                .phase,
            Phase::RolledBack
        );
        assert!(
            read::<Claim>(&fixture.source_directory(), CLAIM, LIMIT)
                .unwrap()
                .released
        );
        fixture.destination.with_locked(admit_enable).unwrap();
        assert!(fixture
            .inventory
            .bind_existing(
                &fixture.destination,
                &fixture.root.join("host"),
                crate::enrollment::OwnerStorage::HeadlessProtectedFile
            )
            .is_err());
        assert!(
            festerm_sessiond::host_control::SessionControl::existing_only(
                &fixture.root.join("host/daemon")
            )
            .unwrap()
            .switch_binding(binding, |_| Ok(()))
            .is_err()
        );
        assert!(festerm_sessiond::list_running_sessions_strict_in(runtime)
            .unwrap()
            .is_empty());
        println!("ordinary native shell exit: phase={} pid={} generation={}; exact key/lease/socket removed; Busy/corrupt proof pending; authoritative absence releases claims/unblocks enable; no rebind/duplicate shell",
            if active { "active" } else { "prepared" }, selected.pid, selected.created_at_unix_ms);
    }

    struct NativeCleanup {
        helper: PathBuf,
        runtime: PathBuf,
        selected: festerm_sessiond::UnattachedSession,
        log: File,
    }

    impl Drop for NativeCleanup {
        fn drop(&mut self) {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match festerm_sessiond::list_unattached_sessions_in(&self.runtime) {
                    Ok(sessions) if sessions.iter().any(|session| session == &self.selected) => {
                        break
                    }
                    Ok(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                    Ok(_) => return,
                    Err(error) => {
                        eprintln!("owned native fixture cleanup observation failed: {error}");
                        return;
                    }
                }
            }
            let mut command = Command::new(&self.helper);
            command
                .env("FESTERM_SESSIOND_RUNTIME_ROOT", &self.runtime)
                .args([
                    "kill-unattached",
                    "--name",
                    &self.selected.name,
                    "--pid",
                    &self.selected.pid.to_string(),
                    "--created-at",
                    &self.selected.created_at_unix_ms.to_string(),
                    "--endpoint",
                    &self.selected.endpoint,
                ])
                .stdin(Stdio::null())
                .stdout(self.log.try_clone().unwrap())
                .stderr(self.log.try_clone().unwrap());
            let _ = bounded(&mut command);
        }
    }

    let helper = PathBuf::from(
        std::env::var_os("FESTERM_ADR84_MIGRATION_HELPER")
            .expect("supply the absolute current-source helper path"),
    );
    assert!(helper.is_absolute() && helper.is_file());
    let host = PathBuf::from(
        std::env::var_os("FESTERM_ADR84_MIGRATION_HOST")
            .expect("supply the absolute current-source host CLI path"),
    );
    assert!(host.is_absolute() && host.is_file());
    let mut fixture = Fixture::new(false);
    fixture.retain = true;
    fixture.inventory.native_evidence = None;
    fixture.inventory.legacy_runtime = fixture.root.join("sessiond");
    storage::private_directory(&fixture.inventory.legacy_runtime, true).unwrap();
    fixture.change("installation.json", |record| {
        record["runtime_root"] = json!(fixture.inventory.legacy_runtime);
    });
    let runtime = &fixture.inventory.legacy_runtime;
    let installation = fixture.selection.source.installation();
    let name = format!(
        "festerm-{}",
        Uuid::parse_str(installation).unwrap().simple()
    );
    let log_directory = storage::private_directory(&fixture.root, false).unwrap();
    let log =
        storage::open_private_file(&log_directory, Path::new("native-command.log"), true).unwrap();
    let mut start = Command::new(&helper);
    start
        .env("FESTERM_SESSIOND_RUNTIME_ROOT", runtime)
        .env_remove("FESTERM_SESSIOND_HOST_CONTROL")
        .env_remove("FESTERM_SESSIOND_GENERATION")
        .env_remove("FESTERM_SESSIOND_LAUNCH_REQUEST")
        .env_remove("FESTERM_SESSIOND_EXPECTED_SPAWN")
        .args([
            "start",
            "--name",
            &name,
            "--shell",
            "/bin/sh",
            "--arg",
            "-c",
            "--arg",
            "while IFS= read -r line; do if [ \"$line\" = 'MIGRATION-EXIT' ]; then exit 0; fi; printf 'MIGRATION-INPUT:%s\\n' \"$line\"; done",
            "--cwd",
        ])
        .arg(&fixture.root)
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log.try_clone().unwrap());
    assert!(
        bounded(&mut start),
        "fixture retained: {}",
        fixture.root.display()
    );
    let selected = festerm_sessiond::list_unattached_sessions_in(runtime)
        .unwrap()
        .pop()
        .unwrap();
    let _native_cleanup = NativeCleanup {
        helper: helper.clone(),
        runtime: runtime.clone(),
        selected: selected.clone(),
        log: log.try_clone().unwrap(),
    };
    fixture.change("installation.json", |record| {
        record["target"] = json!({
            "name": selected.name, "pid": selected.pid, "generation": selected.created_at_unix_ms,
            "endpoint": selected.endpoint,
        });
    });

    let mut stream = UnixStream::connect(&selected.endpoint).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .write_all(&festerm_sessiond::encode_admission_for(
            selected.pid,
            selected.created_at_unix_ms,
            false,
            frontend,
            768 * 1024 * 1024,
        ))
        .unwrap();
    let mut admitted = [0];
    stream.read_exact(&mut admitted).unwrap();
    assert_eq!(admitted, [1]);
    assert!(festerm_sessiond::read_attach_recovery_terminal(
        &mut stream,
        festerm_sessiond::PROTOCOL_VERSION,
        festerm_sessiond::RECOVERY_SNAPSHOT_SCHEMA_VERSION,
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap()
    .is_some());
    stream.write_all(b"FSD1").unwrap();
    stream.write_all(&[4, 0, 0, 0, 0]).unwrap();
    input(&mut stream, b"before\n");
    marker(&mut stream, b"MIGRATION-INPUT:before");
    let before = festerm_sessiond::list_running_sessions_strict_in(runtime).unwrap();
    assert_eq!(before[0].attached_by, Some(frontend));
    let runtime_directory = storage::private_directory(runtime, false).unwrap();
    let capability_name = format!(
        "native-binding-{}-{}.key",
        selected.pid, selected.created_at_unix_ms
    );
    let capability_digest = Sha256::digest(
        &*storage::read_private(&runtime_directory, Path::new(&capability_name), 4096).unwrap(),
    );
    let source_bytes = raw(&fixture.source_directory(), "installation.json").unwrap();
    write(
        &log_directory,
        "selection.json",
        &vec![fixture.selection.clone()],
    );
    let cli = |command: &str, extra: &[&str]| {
        let mut process = Command::new(&host);
        process
            .env("HOME", &fixture.root)
            .env("FESTERM_SESSIOND_RUNTIME_ROOT", runtime)
            .arg(command);
        if matches!(
            command,
            "migration-inventory" | "migration-preview" | "migration-reserve"
        ) {
            process
                .arg("--selection")
                .arg(fixture.root.join("selection.json"))
                .args(["--account-id", "42"]);
        }
        if command != "migration-inventory" {
            process.arg("--state-dir").arg(fixture.root.join("host"));
        }
        let output = process.args(extra).output().unwrap();
        assert!(
            output.status.success(),
            "CLI {command}, fixture {}: {}",
            fixture.root.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let cli_inventory = cli("migration-inventory", &[]);
    assert_eq!(
        cli_inventory[0]["availability"],
        if cfg!(feature = "owner-enrollment") {
            "authenticated-binding-available"
        } else {
            "authenticated-binding-unavailable"
        }
    );
    let preview = fixture.preview();
    assert_eq!(
        cli("migration-preview", &[])["preview_id"],
        preview.preview_id
    );
    assert_eq!(
        preview.mappings[0].availability,
        if cfg!(feature = "owner-enrollment") {
            Availability::AuthenticatedBindingAvailable
        } else {
            Availability::AuthenticatedBindingUnavailable
        }
    );
    let reserved = fixture.reserve();
    let cli_reserved = cli(
        "migration-reserve",
        &["--preview-id", &preview.preview_id, "--consent", "reserve"],
    );
    assert_eq!(cli_reserved["operation"], reserved.operation);
    cli("migration-defer", &[]);
    let deferred = status(&fixture.destination).unwrap();
    assert_eq!(deferred.phase, Phase::Deferred);
    assert_eq!(
        cli("migration-resume", &[])["operation"],
        reserved.operation
    );
    assert_eq!(
        fixture.inventory.resume(&fixture.destination).unwrap(),
        deferred
    );
    input(&mut stream, b"after-reservation\n");
    marker(&mut stream, b"MIGRATION-INPUT:after-reservation");
    assert_eq!(
        source_bytes,
        raw(&fixture.source_directory(), "installation.json").unwrap()
    );
    #[cfg(feature = "owner-enrollment")]
    if _binding {
        use festerm_sessiond::{
            host_control::SessionControl,
            native_binding::{self, BindingPhase},
        };
        let storage = ["--owner-storage", "headless-protected-file"];
        cli(
            "migration-owner-init",
            &[
                "--operation",
                &reserved.operation,
                "--account-id",
                "42",
                storage[0],
                storage[1],
                "--consent",
                "initialize-owner",
            ],
        );
        let owners = fixture
            .destination
            .owner_enrollment_with_storage(crate::enrollment::OwnerStorage::HeadlessProtectedFile)
            .unwrap();
        let initialized = owners.snapshot().unwrap();
        assert!(initialized.keys.is_empty());
        assert!(!initialized.remote_access);
        assert_eq!(
            initialized.capabilities,
            festerm_host_protocol::Capabilities::default()
        );
        let key = [9u8; 32];
        let public = ed25519_dalek::SigningKey::from_bytes(&key)
            .verifying_key()
            .to_bytes();
        let hex = public
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let fingerprint = crate::enrollment::owner_key_fingerprint(&public).unwrap();
        cli(
            "migration-owner-approve",
            &[
                "--operation",
                &reserved.operation,
                "--public-key",
                &hex,
                "--fingerprint",
                &fingerprint,
                storage[0],
                storage[1],
                "--consent",
                "approve",
            ],
        );
        cli(
            "migration-bind",
            &[storage[0], storage[1], "--consent", "bind"],
        );
        let prepared = binding_status(&fixture.destination).unwrap();
        assert_eq!(prepared.bound.len(), 1);
        assert!(prepared.switched.is_empty());
        assert!(!prepared.switch_requested);
        let binding = prepared.bindings.values().next().unwrap();
        assert_eq!(binding.identity.pid, selected.pid);
        assert_eq!(
            binding.identity.created_at_unix_ms,
            selected.created_at_unix_ms
        );
        assert_eq!(&binding.runtime_root, runtime);
        assert_eq!(
            native_binding::probe(runtime, &binding.identity, &binding.spawn_fingerprint)
                .unwrap()
                .unwrap()
                .phase,
            BindingPhase::Prepared
        );
        input(&mut stream, b"after-binding\n");
        marker(&mut stream, b"MIGRATION-INPUT:after-binding");
        if _exit_after_switch == Some(false) {
            exit_and_rollback(&fixture, &mut stream, &selected, false);
            fixture.retain = false;
            return;
        }
        let control_directory =
            storage::private_directory(&fixture.root.join("host/daemon"), false).unwrap();
        let mut interrupted: serde_json::Value =
            serde_json::from_str(&raw(&control_directory, "host-sessions.json").unwrap()).unwrap();
        interrupted["sessions"][binding.logical.as_str()]["binding"]["acknowledged"] = json!(false);
        write(&control_directory, "host-sessions.json", &interrupted);
        let mut interrupted = prepared.clone();
        interrupted.bound.clear();
        write(
            &storage::private_directory(&fixture.root.join("host"), false).unwrap(),
            "host-migration-bindings.json",
            &interrupted,
        );
        cli("migration-resume", &storage);
        assert_eq!(binding_status(&fixture.destination).unwrap(), prepared);
        input(&mut stream, b"after-lost-checkpoint-resume\n");
        marker(&mut stream, b"MIGRATION-INPUT:after-lost-checkpoint-resume");
        cli("migration-defer", &[]);
        assert_eq!(status(&fixture.destination).unwrap().phase, Phase::Deferred);
        cli("migration-resume", &storage);
        assert!(binding_status(&fixture.destination)
            .unwrap()
            .switched
            .is_empty());
        let sessions = SessionControl::existing_only(&fixture.root.join("host/daemon")).unwrap();
        let mut foreign = binding.clone();
        foreign.namespace = festerm_host_protocol::OperationId::new();
        assert!(sessions.switch_binding(&foreign, |_| Ok(())).is_err());
        foreign = binding.clone();
        foreign.principal = festerm_host_protocol::PrincipalId::new();
        assert!(sessions.switch_binding(&foreign, |_| Ok(())).is_err());
        let mut stale = binding.identity.clone();
        stale.created_at_unix_ms += 1;
        assert!(native_binding::probe(runtime, &stale, &binding.spawn_fingerprint).is_err());
        let lease_lock = festerm_sessiond::private_storage::open_private_file(
            &festerm_sessiond::private_storage::private_directory(runtime, false).unwrap(),
            Path::new(&format!("host-daemon-{}.lock", selected.created_at_unix_ms)),
            false,
        )
        .unwrap();
        fs2::FileExt::try_lock_exclusive(&lease_lock).unwrap();
        assert!(fixture
            .inventory
            .switch_existing(
                &fixture.destination,
                &fixture.root.join("host"),
                crate::enrollment::OwnerStorage::HeadlessProtectedFile
            )
            .is_err());
        drop(lease_lock);
        input(&mut stream, b"after-uncertain\n");
        marker(&mut stream, b"MIGRATION-INPUT:after-uncertain");
        cli(
            "migration-switch",
            &[storage[0], storage[1], "--consent", "switch"],
        );
        let switched = binding_status(&fixture.destination).unwrap();
        assert_eq!(switched.switched.len(), 1);
        assert!(switched.switch_requested);
        assert_eq!(
            native_binding::probe(runtime, &binding.identity, &binding.spawn_fingerprint)
                .unwrap()
                .unwrap()
                .phase,
            BindingPhase::Active
        );
        assert_eq!(status(&fixture.destination).unwrap().phase, Phase::Switched);
        let inventory = sessions
            .inventory(&binding.principal, |_| {
                owners
                    .migration_policy(&reserved.operation, false)
                    .map_err(|_| festerm_sessiond::host_control::ControlError::Denied)
            })
            .unwrap();
        assert_eq!(inventory.sessions.len(), 1);
        assert!(inventory.unknown.is_empty());
        assert_eq!(inventory.sessions[0].grant.logical, binding.logical);
        assert!(
            !owners
                .current_policy()
                .unwrap()
                .verifier()
                .policy()
                .remote_access
        );
        cli("migration-resume", &storage);
        assert_eq!(binding_status(&fixture.destination).unwrap(), switched);
        input(&mut stream, b"after-switch-resume\n");
        marker(&mut stream, b"MIGRATION-INPUT:after-switch-resume");
        assert_eq!(
            before,
            festerm_sessiond::list_running_sessions_strict_in(runtime).unwrap()
        );
        assert_eq!(
            status(&fixture.destination).unwrap().destination_host_id,
            reserved.destination_host_id
        );
        if _exit_after_switch == Some(true) {
            exit_and_rollback(&fixture, &mut stream, &selected, true);
            fixture.retain = false;
            return;
        }
    }
    fixture.change("installation.json", |record| {
        record["target"]["generation"] = json!(selected.created_at_unix_ms + 1);
        record["target"]["endpoint"] = json!(runtime.join(format!(
            "{}-{}.sock",
            selected.pid,
            selected.created_at_unix_ms + 1
        )));
    });
    assert!(fixture.inventory.resume(&fixture.destination).is_err());
    #[cfg(feature = "owner-enrollment")]
    if _binding {
        use festerm_sessiond::{
            host_control::SessionControl,
            native_binding::{self, BindingPhase},
        };
        let journal = binding_status(&fixture.destination).unwrap();
        let binding = journal.bindings.values().next().unwrap();
        let sessions = SessionControl::existing_only(&fixture.root.join("host/daemon")).unwrap();
        assert!(sessions.inventory(&binding.principal, |_| Ok(())).is_err());
        assert_eq!(
            native_binding::probe(runtime, &binding.identity, &binding.spawn_fingerprint)
                .unwrap()
                .unwrap()
                .phase,
            BindingPhase::Revoked
        );
        assert!(sessions.switch_binding(binding, |_| Ok(())).is_err());
        input(&mut stream, b"after-revocation\n");
        marker(&mut stream, b"MIGRATION-INPUT:after-revocation");
        fixture
            .inventory
            .release_bindings(&fixture.destination, &fixture.root.join("host"))
            .unwrap();
        input(&mut stream, b"after-release\n");
        marker(&mut stream, b"MIGRATION-INPUT:after-release");
        assert!(
            runtime
                .join(format!(
                    "native-binding-{}-{}.key",
                    selected.pid, selected.created_at_unix_ms
                ))
                .exists(),
            "live release removed the server capability"
        );
        let mut interrupted = binding_status(&fixture.destination).unwrap();
        interrupted.released.clear();
        write(
            &storage::private_directory(&fixture.root.join("host"), false).unwrap(),
            "host-migration-bindings.json",
            &interrupted,
        );
        cli("migration-resume", &[]);
        assert_eq!(
            status(&fixture.destination).unwrap().phase,
            Phase::RolledBack
        );
    }
    assert_eq!(
        cli("migration-rollback", &["--consent", "rollback"])["phase"],
        "rolled-back"
    );
    let rollback = fixture.inventory.rollback(&fixture.destination).unwrap();
    assert_eq!(rollback.destination_host_id, reserved.destination_host_id);
    input(&mut stream, b"after-rollback\n");
    marker(&mut stream, b"MIGRATION-INPUT:after-rollback");
    assert_eq!(
        capability_digest,
        Sha256::digest(
            &*storage::read_private(&runtime_directory, Path::new(&capability_name), 4096).unwrap(),
        ),
        "binding/switch/resume/revocation/release/rollback changed the live capability",
    );
    assert_eq!(
        before,
        festerm_sessiond::list_running_sessions_strict_in(runtime).unwrap()
    );
    assert_ne!(
        source_bytes,
        raw(&fixture.source_directory(), "installation.json").unwrap()
    );
    assert!(runtime
        .join(format!(
            "lease-{}-{}",
            selected.pid, selected.created_at_unix_ms
        ))
        .exists());
    if !_binding {
        assert!(!fixture.root.join("host").join("daemon").exists());
    }
    #[cfg(feature = "owner-enrollment")]
    if _binding {
        use festerm_sessiond::native_binding::{self, BindingPhase};
        let released = binding_status(&fixture.destination).unwrap();
        assert_eq!(released.released.len(), 1);
        let binding = released.bindings.values().next().unwrap();
        assert_eq!(
            native_binding::probe(runtime, &binding.identity, &binding.spawn_fingerprint)
                .unwrap()
                .unwrap()
                .phase,
            BindingPhase::Released
        );
        assert!(fixture
            .inventory
            .bind_existing(
                &fixture.destination,
                &fixture.root.join("host"),
                crate::enrollment::OwnerStorage::HeadlessProtectedFile
            )
            .is_err());
        println!("authenticated current-source native binding: pid={} generation={} frontend={:?} namespace={}; prepared/switch/resume/revoked/released; retained runtime={}; live capability unchanged; no stop/restart/frontend acquisition", selected.pid, selected.created_at_unix_ms, before[0].attached_by, binding.namespace.as_str(), runtime.display());
    }
    println!(
            "current-source native fixture: helper={} host={} pid={} generation={} frontend={:?}; reservation={} retained-host={:?}; reservation-only fixture does not establish remote endpoint readiness",
            helper.display(), host.display(), selected.pid, selected.created_at_unix_ms,
            before[0].attached_by, reserved.operation, reserved.destination_host_id,
        );
    drop(stream);

    let deadline = Instant::now() + Duration::from_secs(5);
    while festerm_sessiond::list_unattached_sessions_in(runtime)
        .unwrap()
        .is_empty()
    {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    let mut cleanup = Command::new(&helper);
    cleanup
        .env("FESTERM_SESSIOND_RUNTIME_ROOT", runtime)
        .args([
            "kill-unattached",
            "--name",
            &selected.name,
            "--pid",
            &selected.pid.to_string(),
            "--created-at",
            &selected.created_at_unix_ms.to_string(),
            "--endpoint",
            &selected.endpoint,
        ])
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log);
    assert!(
        bounded(&mut cleanup),
        "fixture retained: {}",
        fixture.root.display()
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !festerm_sessiond::list_running_sessions_strict_in(runtime)
        .unwrap()
        .is_empty()
    {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!runtime
        .join(format!(
            "native-binding-{}-{}.key",
            selected.pid, selected.created_at_unix_ms
        ))
        .exists());
    fixture.retain = false;
}

fn write(directory: &File, name: &str, value: &impl Serialize) {
    storage::replace_private(directory, Path::new(name), &encode(value).unwrap()).unwrap();
}

#[test]
fn owned_migration_explicit_inventory_preview_and_idempotent_reservation_preserve_receipts() {
    for desktop in [false, true] {
        let fixture = Fixture::new(desktop);
        let before = raw(&fixture.source_directory(), "installation.json").unwrap();
        let preview = fixture.preview();
        assert!(!preview.daemon_binding_supported);
        assert!(!preview.mappings[0].live_stop_authorized);
        assert_eq!(
            preview.mappings[0].availability,
            Availability::AuthenticatedBindingUnavailable
        );
        assert!(!fixture.root.join("host").join(HOST_IDENTITY).exists());
        let reserved = fixture.reserve();
        assert_eq!(reserved.phase, Phase::AwaitingNativeBinding);
        assert_ne!(
            reserved.destination_host_id.as_deref(),
            Some(preview.mappings[0].original_host_id.as_str())
        );
        assert_eq!(reserved, fixture.reserve());
        assert_eq!(
            reserved,
            fixture.inventory.resume(&fixture.destination).unwrap()
        );
        assert_eq!(reserved, status(&fixture.destination).unwrap());
        assert_eq!(
            before,
            raw(&fixture.source_directory(), "installation.json").unwrap()
        );
        assert_eq!(fixture.destination.snapshot().unwrap(), preview.destination);
        assert!(fixture
            .destination
            .request_enable(&preview.destination.settings)
            .is_err());
        let deferred = fixture.inventory.defer(&fixture.destination).unwrap();
        assert_eq!(deferred.phase, Phase::Deferred);
        assert_eq!(
            deferred,
            fixture.inventory.resume(&fixture.destination).unwrap()
        );
    }
}

#[test]
fn owned_migration_refuses_foreign_account_key_name_collision_and_untrusted_receipts() {
    let fixture = Fixture::new(true);
    assert!(fixture
        .inventory
        .inventory(std::slice::from_ref(&fixture.selection), "43")
        .is_err());
    let mut foreign = fixture.selection.clone();
    if let Source::DesktopProfile { installation } = &mut foreign.source {
        *installation = Uuid::new_v4().to_string();
    }
    assert!(fixture.inventory.inventory(&[foreign], "42").is_err());
    let mut collision = fixture.selection.clone();
    collision.logical_id = Uuid::new_v4().to_string();
    assert!(fixture
        .inventory
        .inventory(&[fixture.selection.clone(), collision], "42")
        .is_err());
    fixture.change("installation.json", |record| {
        record["fingerprint"] = json!("foreign-key")
    });
    assert!(fixture
        .inventory
        .inventory(std::slice::from_ref(&fixture.selection), "42")
        .is_err());
    assert!(!fixture.root.join("host").join(INTENT).exists());
}

#[test]
fn owned_migration_reconfirms_changed_metadata_and_refuses_stale_epoch() {
    let fixture = Fixture::new(false);
    let preview = fixture.preview();
    let mut changed = fixture.selection.clone();
    changed.label = "another label".into();
    assert!(fixture
        .inventory
        .reserve(&fixture.destination, &[changed], "42", &preview.preview_id)
        .is_err());
    let reserved = fixture.reserve();
    fixture.destination.revoke().unwrap();
    assert!(fixture.inventory.resume(&fixture.destination).is_err());
    assert_eq!(status(&fixture.destination).unwrap(), reserved);
    let rolled = fixture.inventory.rollback(&fixture.destination).unwrap();
    assert_eq!(rolled.phase, Phase::RolledBack);
}

#[test]
fn owned_migration_rollback_releases_only_own_claims_never_resurrects_revocation() {
    let fixture = Fixture::new(true);
    let reserved = fixture.reserve();
    let grant = profile_hosting::Store::open(
        &fixture.source,
        fixture.selection.source.installation(),
        false,
    )
    .unwrap();
    let consent = grant.read().unwrap().consent;
    grant.revoke(&consent).unwrap();
    assert!(fixture.inventory.resume(&fixture.destination).is_err());
    let revoked = raw(&fixture.source_directory(), "profile-consent.json").unwrap();
    let rolled = fixture.inventory.rollback(&fixture.destination).unwrap();
    assert_eq!(rolled.phase, Phase::RolledBack);
    assert_eq!(rolled.destination_host_id, reserved.destination_host_id);
    assert_eq!(
        rolled,
        fixture.inventory.rollback(&fixture.destination).unwrap()
    );
    assert_eq!(
        revoked,
        raw(&fixture.source_directory(), "profile-consent.json").unwrap()
    );
    assert!(fixture.inventory.resume(&fixture.destination).is_err());
    let snapshot = fixture.destination.snapshot().unwrap();
    fixture
        .destination
        .request_enable(&snapshot.settings)
        .unwrap();
}

#[test]
fn owned_migration_missing_corrupt_foreign_and_released_claims_refuse_resume() {
    for corruption in ["missing", "corrupt", "foreign", "released"] {
        let fixture = Fixture::new(false);
        fixture.reserve();
        if corruption == "missing" {
            std::fs::remove_file(fixture.source.join(CLAIM)).unwrap();
        } else if corruption == "corrupt" {
            storage::replace_private(&fixture.source_directory(), Path::new(CLAIM), b"{").unwrap();
        } else {
            fixture.change(CLAIM, |claim| {
                if corruption == "released" {
                    claim["released"] = json!(true);
                } else {
                    claim["operation"] = json!(Uuid::new_v4().to_string());
                }
            });
        }
        assert!(
            fixture.inventory.resume(&fixture.destination).is_err(),
            "{corruption}"
        );
        assert!(!fixture.source.join("daemon-created").exists());
    }
}

#[test]
fn owned_migration_crash_before_identity_and_after_claim_resumes_without_duplicate_identity() {
    let fixture = Fixture::new(false);
    let preview = fixture.preview();
    let sources = fixture
        .inventory
        .load_all(std::slice::from_ref(&fixture.selection), "42")
        .unwrap();
    let intent = Intent {
        version: 1,
        operation: Uuid::new_v4().to_string(),
        preview_id: preview.preview_id,
        plan: plan(&preview.destination, "42", &sources),
    };
    drop(sources);
    fixture
        .destination
        .with_locked(|directory, _| {
            protected_state::publish_new(directory, Path::new(INTENT), &encode(&intent)?)
        })
        .unwrap();
    let resumed = fixture.inventory.resume(&fixture.destination).unwrap();
    fixture
        .destination
        .with_locked(|directory, _| {
            let mut journal: Journal = read(directory, JOURNAL, LIMIT)?;
            journal.phase = Phase::Reserving;
            journal.claimed_sources.clear();
            save_journal(directory, &journal)
        })
        .unwrap();
    let retried = fixture.inventory.resume(&fixture.destination).unwrap();
    assert_eq!(resumed, retried);
    assert_eq!(resumed.operation, intent.operation);
}

#[test]
fn owned_migration_intent_only_rollback_never_creates_identity_or_claims() {
    let fixture = Fixture::new(false);
    let preview = fixture.preview();
    let sources = fixture
        .inventory
        .load_all(std::slice::from_ref(&fixture.selection), "42")
        .unwrap();
    let intent = Intent {
        version: 1,
        operation: Uuid::new_v4().to_string(),
        preview_id: preview.preview_id,
        plan: plan(&preview.destination, "42", &sources),
    };
    drop(sources);
    fixture
        .destination
        .with_locked(|directory, _| {
            protected_state::publish_new(directory, Path::new(INTENT), &encode(&intent)?)
        })
        .unwrap();
    let rolled = fixture.inventory.rollback(&fixture.destination).unwrap();
    assert_eq!(rolled.phase, Phase::RolledBack);
    assert!(rolled.destination_host_id.is_none());
    assert!(!fixture.root.join("host").join(HOST_IDENTITY).exists());
    assert!(!fixture.source.join(CLAIM).exists());
    assert!(fixture.inventory.resume(&fixture.destination).is_err());
    assert!(!fixture.root.join("host").join(HOST_IDENTITY).exists());
    fixture
        .destination
        .with_locked(|directory, snapshot| {
            admit_enable(directory, snapshot)?;
            HostIdentity::load_or_create_from_private_directory(
                directory,
                Path::new(HOST_IDENTITY),
            )?;
            admit_enable(directory, snapshot)
        })
        .unwrap();
    assert_eq!(status(&fixture.destination).unwrap(), rolled);
}

#[test]
fn owned_migration_partial_multi_source_reservation_and_rollback_preserve_foreign_claims() {
    let fixture = Fixture::new(false);
    let desktop = Fixture::new(true);
    let runtime = &fixture.inventory.legacy_runtime;
    desktop.change("installation.json", |record| {
        record["runtime_root"] = json!(runtime);
        record["target"]["endpoint"] = json!(runtime.join("123-456.sock"));
    });
    desktop.change("profile-consent.json", |record| {
        record["target"]["runtime_root"] = json!(runtime);
        record["target"]["endpoint"] = json!(runtime.join("123-456.sock"));
    });
    let moved = fixture
        .inventory
        .desktop
        .join("installations")
        .join(desktop.selection.source.installation());
    std::fs::rename(&desktop.source, &moved).unwrap();
    let values = [fixture.selection.clone(), desktop.selection.clone()];
    let preview = fixture
        .inventory
        .preview(&fixture.destination, &values, "42")
        .unwrap();
    assert_eq!(preview.mappings.len(), 2);
    assert_eq!(
        preview.mappings[0].selection.label,
        preview.mappings[1].selection.label
    );
    let row = preview
        .mappings
        .iter()
        .find(|row| row.selection.source == fixture.selection.source)
        .unwrap();
    let foreign = Claim {
        version: 1,
        source: fixture.selection.source.clone(),
        source_host_id: row.original_host_id.clone(),
        source_key_fingerprint: row.original_key_fingerprint.clone(),
        source_receipt_digest: "a".repeat(64),
        destination_installation: Uuid::new_v4().to_string(),
        destination_host_id: Uuid::new_v4().to_string(),
        destination_key_fingerprint: "foreign-protected-key".into(),
        operation: Uuid::new_v4().to_string(),
        released: false,
    };
    write(&fixture.source_directory(), CLAIM, &foreign);
    assert!(fixture
        .inventory
        .reserve(&fixture.destination, &values, "42", &preview.preview_id)
        .is_err());
    let partial = status(&fixture.destination).unwrap();
    assert_eq!(partial.phase, Phase::Reserving);
    assert_eq!(partial.claimed_sources.len(), 1);
    assert!(fixture.inventory.resume(&fixture.destination).is_err());
    let foreign_before = raw(&fixture.source_directory(), CLAIM).unwrap();
    let rolled = fixture.inventory.rollback(&fixture.destination).unwrap();
    assert_eq!(rolled.phase, Phase::RolledBack);
    assert_eq!(rolled.destination_host_id, partial.destination_host_id);
    assert_eq!(
        foreign_before,
        raw(&fixture.source_directory(), CLAIM).unwrap()
    );
    let moved_directory = storage::private_directory(&moved, false).unwrap();
    let released: Claim = read(&moved_directory, CLAIM, SOURCE_LIMIT).unwrap();
    assert!(released.released);
    assert_eq!(released.operation, partial.operation);
}

#[test]
fn owned_migration_busy_future_missing_and_corrupt_state_never_becomes_absence() {
    let fixture = Fixture::new(false);
    let preview = fixture.preview();
    let directory = fixture.source_directory();
    let lock = Lease::exclusive(
        storage::open_private_file(&directory, Path::new("installer.lock"), false).unwrap(),
    )
    .unwrap();
    assert!(fixture
        .inventory
        .inventory(std::slice::from_ref(&fixture.selection), "42")
        .is_err());
    drop(lock);
    fixture.reserve();
    fixture
        .destination
        .with_locked(|directory, _| {
            let mut intent: serde_json::Value = read(directory, INTENT, LIMIT)?;
            intent["version"] = json!(999);
            write(directory, INTENT, &intent);
            Ok(())
        })
        .unwrap();
    assert!(fixture.inventory.resume(&fixture.destination).is_err());
    assert!(fixture.inventory.rollback(&fixture.destination).is_err());
    assert!(fixture
        .destination
        .request_enable(&preview.destination.settings)
        .is_err());
    let identity_before = std::fs::read(fixture.root.join("host").join(HOST_IDENTITY)).unwrap();
    std::fs::remove_file(fixture.root.join("host").join(JOURNAL)).unwrap();
    assert!(fixture.inventory.resume(&fixture.destination).is_err());
    assert_eq!(
        identity_before,
        std::fs::read(fixture.root.join("host").join(HOST_IDENTITY)).unwrap()
    );
}

#[test]
fn owned_migration_checkpointed_claim_and_journal_loss_are_not_recreated() {
    for missing in [JOURNAL, HOST_IDENTITY] {
        let fixture = Fixture::new(false);
        fixture.reserve();
        std::fs::remove_file(fixture.root.join("host").join(missing)).unwrap();
        assert!(
            fixture.inventory.resume(&fixture.destination).is_err(),
            "{missing}"
        );
        assert!(!fixture.root.join("host").join(missing).exists());
    }
}

#[test]
fn owned_migration_rollback_keeps_pending_evidence_on_foreign_source_replacement() {
    let fixture = Fixture::new(false);
    fixture.reserve();
    fixture.change("installation.json", |record| {
        record["account"] = json!("99")
    });
    let claim_before = raw(&fixture.source_directory(), CLAIM).unwrap();
    assert!(fixture.inventory.rollback(&fixture.destination).is_err());
    assert_eq!(
        status(&fixture.destination).unwrap().phase,
        Phase::RollbackPending
    );
    assert_eq!(
        claim_before,
        raw(&fixture.source_directory(), CLAIM).unwrap()
    );
    fixture.change("installation.json", |record| {
        record["account"] = json!("42")
    });
    fixture.change("installation.json", |record| record["version"] = json!(999));
    assert!(fixture.inventory.rollback(&fixture.destination).is_err());
    assert_eq!(
        claim_before,
        raw(&fixture.source_directory(), CLAIM).unwrap()
    );
    fixture.change("installation.json", |record| record["version"] = json!(1));
    assert_eq!(
        fixture
            .inventory
            .rollback(&fixture.destination)
            .unwrap()
            .phase,
        Phase::RolledBack
    );
}

#[test]
fn owned_migration_rejects_pid_only_legacy_and_untrusted_native_registry_without_repair() {
    let mut fixture = Fixture::new(false);
    fixture.inventory.native_evidence = None;
    let runtime = &fixture.inventory.legacy_runtime;
    fixture.change("installation.json", |record| {
        record["target"]["endpoint"] = json!(runtime.join("legacy-name.sock"));
    });
    assert_eq!(
        fixture.preview().mappings[0].availability,
        Availability::NativeGenerationUnavailable
    );
    fixture.change("installation.json", |record| {
        record["target"]["endpoint"] = json!(runtime.join("123-456.sock"));
    });
    let directory = storage::private_directory(runtime, false).unwrap();
    storage::open_private_file(&directory, Path::new("registry.lock"), true).unwrap();
    write(
        &directory,
        "registry.json",
        &json!({"sessions": {"unknown": {"future": true}}}),
    );
    assert!(fixture
        .inventory
        .inventory(std::slice::from_ref(&fixture.selection), "42")
        .is_err());
    assert!(!fixture.root.join("host").join(INTENT).exists());
    assert!(!fixture.source.join(CLAIM).exists());
}

#[test]
fn owned_migration_unavailable_native_cleanup_and_default_no_preserve_legacy() {
    let mut fixture = Fixture::new(false);
    fixture.inventory.native_evidence = Some(|_, _| Ok(false));
    let preview = fixture.preview();
    assert_eq!(
        preview.mappings[0].availability,
        Availability::NativeGenerationUnavailable
    );
    assert!(fixture
        .inventory
        .reserve(
            &fixture.destination,
            std::slice::from_ref(&fixture.selection),
            "42",
            &preview.preview_id
        )
        .is_err());
    assert!(!fixture.root.join("host").join(INTENT).exists());
    write(
        &fixture.source_directory(),
        "deletion.json",
        &json!({"retained": "pending"}),
    );
    assert_eq!(
        fixture.preview().mappings[0].availability,
        Availability::CleanupPending
    );
    let mut selection = serde_json::to_value(&fixture.selection).unwrap();
    selection["stop"] = json!(true);
    assert!(serde_json::from_value::<Selection>(selection).is_err());
}

#[cfg(unix)]
#[test]
fn owned_migration_refuses_symlink_and_nonprivate_source_evidence_without_repair() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let fixture = Fixture::new(false);
    let path = fixture.source.join("script-ownership.json");
    let original = std::fs::read(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(fixture
        .inventory
        .inventory(std::slice::from_ref(&fixture.selection), "42")
        .is_err());
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o644
    );
    std::fs::remove_file(&path).unwrap();
    let other = fixture.root.join("public-receipt.json");
    std::fs::write(&other, original).unwrap();
    symlink(&other, &path).unwrap();
    assert!(fixture
        .inventory
        .inventory(std::slice::from_ref(&fixture.selection), "42")
        .is_err());
}
