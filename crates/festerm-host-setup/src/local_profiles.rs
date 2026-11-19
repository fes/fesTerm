//! Explicit OS-local access to an exact saved HOST catalog profile. This is not
//! a remoting capability and no RPC dispatches these operations.
use crate::{
    enrollment::OwnerEnrollment,
    lifecycle::{Origin, Phase, Snapshot, Store},
    services::Services,
};
use festerm_host_protocol::{
    Epoch, IssuedOperation, LogicalSessionId, LogicalState, OperationId, Recipe, SessionSummary,
};
use festerm_session::TerminalSize;
use festerm_sessiond::{
    host_control::{AdmissionState, ControlError, LocalProfileRequest, SessionControl},
    NativeSessionIdentity,
};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

pub struct Request<'a> {
    pub logical: &'a LogicalSessionId,
    pub consent: &'a str,
    pub expected_revision: Epoch,
    pub expected_recipe: &'a Recipe,
    pub expected_enabled: bool,
    pub allow_start: bool,
    pub size: TerminalSize,
    pub scrollback_limit_bytes: usize,
}

pub struct Target {
    pub runtime_root: PathBuf,
    pub identity: NativeSessionIdentity,
    pub summary: SessionSummary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Cancelled,
    Stale,
    Unavailable,
    Busy,
    StartRequired,
    StartContextBlocked,
    StartContextUnavailable,
    Unknown,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Cancelled=>"Local profile connection cancelled.",
            Self::Stale=>"The saved profile, recipe or host authority changed. No sibling shell was started.",
            Self::Unavailable=>"The protected profile or authenticated native helper is unavailable. No fallback shell was started.",
            Self::Busy=>"The exact profile operation is still busy. No replacement shell was started.",
            Self::StartRequired=>"This profile has no running generation. An explicit local Connect is required to start it.",
            Self::StartContextBlocked=>"This Windows launch context cannot start an independent saved session. Open fesTerm normally to start it. Existing running sessions can still be attached.",
            Self::StartContextUnavailable=>"Could not verify independent startup context. No shell was started.",
            Self::Unknown=>"The existing profile outcome is uncertain. No replacement shell was started.",
        })
    }
}
impl std::error::Error for Error {}
fn control(error: ControlError) -> Error {
    match error {
        ControlError::Busy => Error::Busy,
        ControlError::Cancelled => Error::Cancelled,
        ControlError::Stale | ControlError::RecipeConflict | ControlError::OperationConflict => {
            Error::Stale
        }
        ControlError::Unknown | ControlError::CommittedButNotPublished | ControlError::Ended => {
            Error::Unknown
        }
        _ => Error::Unavailable,
    }
}
fn local_request<'a>(request: &'a Request<'a>) -> LocalProfileRequest<'a> {
    LocalProfileRequest {
        logical: request.logical,
        consent: request.consent,
        revision: request.expected_revision,
        recipe: request.expected_recipe,
        enabled: request.expected_enabled,
        size: request.size,
        scrollback_limit_bytes: request.scrollback_limit_bytes,
    }
}

pub fn ensure(
    store: &Store,
    state: &Path,
    request: Request<'_>,
    cancel: &AtomicBool,
) -> Result<Target, Error> {
    let owners = store.owner_enrollment().map_err(|_| Error::Unavailable)?;
    ensure_using(&owners, state, request, cancel, |expected| {
        Services::current_user(store, state)
            .and_then(|services| services.reopen_local_profile_stage(expected.epoch))
            .and_then(|stage| stage.sessiond_path().map(Path::to_path_buf))
            .map_err(|_| Error::Unavailable)
    })
}

fn ensure_using(
    owners: &OwnerEnrollment<'_>,
    state: &Path,
    request: Request<'_>,
    cancel: &AtomicBool,
    resolve: impl FnOnce(&Snapshot) -> Result<PathBuf, Error>,
) -> Result<Target, Error> {
    ensure_using_with_start_check(
        owners,
        state,
        request,
        cancel,
        resolve,
        festerm_sessiond::independent_host_start_blocked,
    )
}

fn ensure_using_with_start_check(
    owners: &OwnerEnrollment<'_>,
    state: &Path,
    request: Request<'_>,
    cancel: &AtomicBool,
    resolve: impl FnOnce(&Snapshot) -> Result<PathBuf, Error>,
    start_blocked: impl FnOnce() -> std::io::Result<bool>,
) -> Result<Target, Error> {
    if cancel.load(Ordering::Acquire) {
        return Err(Error::Cancelled);
    }
    request
        .expected_recipe
        .validate()
        .map_err(|_| Error::Stale)?;
    // OS-local inspection is noninteractive and writes no lifecycle high-water,
    // owner key, cloud resource, service state or remote grant.
    let initial = owners
        .local_session_policy_noninteractive(state)
        .map_err(|_| Error::Unavailable)?;
    if initial.installation.origin != Origin::Desktop
        || !matches!(
            initial.installation.phase,
            Phase::Disabled | Phase::Provisioned
        )
    {
        return Err(Error::Unavailable);
    }
    let expected = initial.installation.clone();
    let owner = initial.enrollment.clone();
    drop(initial);
    let root = state.join("daemon");
    let sessions = SessionControl::existing_only(&root).map_err(control)?;
    let admit = |admission: &AdmissionState| {
        if cancel.load(Ordering::Acquire) {
            return Err(ControlError::Cancelled);
        }
        let guard = owners
            .local_session_policy_noninteractive(state)
            .map_err(|_| ControlError::Denied)?;
        if guard.installation != expected
            || guard.enrollment.binding != owner.binding
            || guard.enrollment.revision != owner.revision
        {
            return Err(ControlError::Stale);
        }
        if !request.allow_start
            && admission.effect != festerm_sessiond::host_control::Effect::Lookup
            && !admission
                .grant
                .as_ref()
                .is_some_and(|grant| matches!(grant.state, LogicalState::Running(_)))
        {
            return Err(ControlError::Denied);
        }
        Ok(guard)
    };
    // Running lookup is independent of helper/package/service availability.
    // It still proves exact recipe/consent/publication state and native identity.
    if let Some(current) = sessions
        .local_profile_target(&owner.binding.principal, &local_request(&request), admit)
        .map_err(control)?
    {
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        return Ok(Target {
            runtime_root: root,
            identity: current.identity,
            summary: current.summary,
        });
    }
    if !request.allow_start {
        return Err(Error::StartRequired);
    }
    // Only a new start needs this check. Never reinterpret an existing Unknown
    // reservation or prevent attaching an already verified Running generation.
    if start_blocked().map_err(|_| Error::StartContextUnavailable)? {
        return Err(Error::StartContextBlocked);
    }
    let helper = resolve(&expected)?;
    if cancel.load(Ordering::Acquire) {
        return Err(Error::Cancelled);
    }
    let sessions = SessionControl::with_helper(&root, &helper).map_err(control)?;
    let epoch = sessions
        .issuance_epoch(&owner.binding.principal, |_| {
            admit(&AdmissionState {
                grant: None,
                grant_epoch: None,
                effect: festerm_sessiond::host_control::Effect::Lookup,
                issuance_epoch: None,
            })
        })
        .map_err(control)?;
    let issued = IssuedOperation {
        id: OperationId::new(),
        epoch,
    };
    let result = sessions
        .ensure_local_profile(
            &owner.binding.principal,
            &issued,
            local_request(&request),
            cancel,
            admit,
        )
        .map_err(control)?;
    if result.cancelled || cancel.load(Ordering::Acquire) {
        return Err(Error::Cancelled);
    }
    Ok(Target {
        runtime_root: root,
        identity: result.identity,
        summary: result.summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        enrollment::tests::{fixture_owners, setup_origin, Vault},
        lifecycle::{Binding, OrdinaryAccess, Settings, Startup},
    };
    use festerm_sessiond::private_storage as storage;
    use std::sync::Arc;
    fn setup() -> (storage::PrivateTestDirectory, Store, Arc<Vault>) {
        let (_unused, _store, vault) = setup_origin(Origin::Desktop);
        let root = storage::private_test_directory_in(&std::env::current_dir().unwrap());
        let state = root.path().join("s");
        let store = Store::create(
            &state,
            Origin::Desktop,
            Settings {
                ordinary_access: OrdinaryAccess::ProviderOwner,
                friendly_name: "local fixture".into(),
                allow_creation: false,
                startup: Startup::Manual,
            },
        )
        .unwrap();
        let identity = festerm_tunnel_bridge::host_identity::HostIdentity::load_or_create(
            &state.join("host-identity"),
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
                    account: "github:1234".into(),
                    qualified_tunnel: "fixture.invalid/exact".into(),
                    cloud_incarnation: "fixture".into(),
                    host_key_fingerprint: identity.fingerprint().unwrap(),
                },
            )
            .unwrap();
        let owners = fixture_owners(&store, vault.clone());
        owners
            .initialize(Path::new("host-identity"), "github:1234".into())
            .unwrap();
        (root, store, vault)
    }
    fn request<'a>(
        logical: &'a LogicalSessionId,
        recipe: &'a Recipe,
        revision: Epoch,
        enabled: bool,
        allow_start: bool,
    ) -> Request<'a> {
        Request {
            logical,
            consent: "11111111-1111-4111-8111-111111111111",
            expected_revision: revision,
            expected_recipe: recipe,
            expected_enabled: enabled,
            allow_start,
            size: TerminalSize::new(117, 39).unwrap(),
            scrollback_limit_bytes: 4096,
        }
    }

    fn daemon_files(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        fn collect(
            root: &Path,
            path: &Path,
            files: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
        ) {
            for entry in std::fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    collect(root, &path, files);
                } else {
                    files.insert(
                        path.strip_prefix(root).unwrap().into(),
                        std::fs::read(path).unwrap(),
                    );
                }
            }
        }
        let mut files = std::collections::BTreeMap::new();
        collect(root, root, &mut files);
        files
    }

    fn assert_start_context_refuses_before_reservation(
        check: impl FnOnce() -> std::io::Result<bool>,
        expected: Error,
    ) {
        let (root, store, vault) = setup();
        let state = root.path().join("s");
        let owners = fixture_owners(&store, vault);
        let logical = LogicalSessionId::new();
        let recipe = Recipe::default();
        let daemon = state.join("daemon");
        let sessions =
            SessionControl::with_helper(&daemon, &std::env::current_exe().unwrap()).unwrap();
        let revision = sessions
            .set_profile(
                logical.clone(),
                "11111111-1111-4111-8111-111111111111",
                "Local",
                recipe.clone(),
                true,
            )
            .unwrap();
        let before = daemon_files(&daemon);
        assert!(matches!(
            ensure_using_with_start_check(
                &owners, &state, request(&logical, &recipe, revision, true, true),
                &AtomicBool::new(false), |_| panic!("refused context must not resolve or launch helper"), check,
            ),
            Err(error) if error == expected
        ));
        // Includes the exact catalog/operation bytes and absence of launch or
        // generation-lease artifacts, rather than only checking the error label.
        assert_eq!(daemon_files(&daemon), before);
        let catalog: serde_json::Value =
            serde_json::from_slice(&std::fs::read(daemon.join("host-sessions.json")).unwrap())
                .unwrap();
        assert_eq!(catalog["sessions"], serde_json::json!({}));
        assert_eq!(catalog["operations"], serde_json::json!({}));
        assert_eq!(catalog["last_generation"], 0);
    }

    #[test]
    fn local_blocked_start_context_leaves_catalog_journals_leases_and_operations_unchanged() {
        assert_start_context_refuses_before_reservation(|| Ok(true), Error::StartContextBlocked);
        assert!(Error::StartContextBlocked
            .to_string()
            .contains("Existing running sessions can still be attached"));
    }

    #[test]
    fn local_start_context_query_error_is_unavailable_without_reservation() {
        assert_start_context_refuses_before_reservation(
            || Err(std::io::Error::other("context query unavailable")),
            Error::StartContextUnavailable,
        );
    }

    #[test]
    fn local_unknown_reservation_is_not_reclassified_by_start_context() {
        use festerm_host_protocol::{GenerationId, Lifetime};
        let (root, store, vault) = setup();
        let state = root.path().join("s");
        let owners = fixture_owners(&store, vault);
        let principal = owners.snapshot().unwrap().binding.principal;
        let logical = LogicalSessionId::new();
        let recipe = Recipe::default();
        let daemon = state.join("daemon");
        let sessions =
            SessionControl::with_helper(&daemon, &std::env::current_exe().unwrap()).unwrap();
        let revision = sessions
            .set_profile(
                logical.clone(),
                "11111111-1111-4111-8111-111111111111",
                "Local",
                recipe.clone(),
                true,
            )
            .unwrap();
        let file = daemon.join("host-sessions.json");
        let mut catalog: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        catalog["last_generation"] = serde_json::json!(1);
        catalog["sessions"][logical.as_str()] = serde_json::json!({
            "owner": principal, "logical": logical, "display_name": "Local",
            "recipe": recipe, "effective_recipe": recipe, "lifetime": Lifetime::Persistent {},
            "epoch": Epoch::new(1).unwrap(), "revoked": false, "name": "hs-slot-00",
            "launch": OperationId::new(), "generation": GenerationId::new(1).unwrap(),
            "fingerprint": "0".repeat(64), "phase": "Launching", "identity": null,
            "artifacts_retired": false, "binding": null, "profile_revision": revision,
            "profile_enabled": true
        });
        let directory = storage::private_directory(&daemon, false).unwrap();
        // A retained Launching reservation with unavailable lease evidence is
        // Unknown, never authorization to enter a new-start preflight.
        drop(
            storage::open_private_file(&directory, Path::new("host-daemon-1.lock"), true).unwrap(),
        );
        storage::replace_private(
            &directory,
            Path::new("host-sessions.json"),
            &serde_json::to_vec(&catalog).unwrap(),
        )
        .unwrap();
        // Exercise the real protected-catalog lookup; no helper is executed.
        assert_eq!(
            sessions
                .local_profile_target(
                    &principal,
                    &local_request(&request(&logical, &recipe, revision, true, true)),
                    |_| Ok::<_, ControlError>(())
                )
                .map(|target| target.is_some()),
            Err(ControlError::Unknown)
        );
        let before = daemon_files(&daemon);
        assert!(matches!(
            ensure_using_with_start_check(
                &owners,
                &state,
                request(&logical, &recipe, revision, true, true),
                &AtomicBool::new(false),
                |_| panic!("Unknown cannot resolve helper"),
                || panic!("Unknown must return before any new-start context check"),
            ),
            Err(Error::Unknown)
        ));
        assert_eq!(daemon_files(&daemon), before);
    }
    #[test]
    fn local_restore_never_prompts_writes_or_resolves_helper_and_locked_store_refuses() {
        let (root, store, vault) = setup();
        let state = root.path().join("s");
        let owners = fixture_owners(&store, vault.clone());
        let principal = owners.snapshot().unwrap().binding.principal;
        let logical = LogicalSessionId::new();
        let recipe = Recipe::default();
        let sessions =
            SessionControl::with_helper(&state.join("daemon"), &std::env::current_exe().unwrap())
                .unwrap();
        let revision = sessions
            .set_profile(
                logical.clone(),
                "11111111-1111-4111-8111-111111111111",
                "Local",
                recipe.clone(),
                false,
            )
            .unwrap();
        store.revoke().unwrap();
        let stopped = store.snapshot().unwrap();
        store
            .confirm_stopped(stopped.epoch, stopped.binding.as_ref().unwrap())
            .unwrap();
        let before = std::fs::read(state.join("owner-enrollment.json")).unwrap();
        let reads = vault.interactive_reads.load(Ordering::SeqCst);
        assert!(matches!(
            ensure_using(
                &owners,
                &state,
                request(&logical, &recipe, revision, false, false),
                &AtomicBool::new(false),
                |_| panic!("restore cannot load helper")
            ),
            Err(Error::StartRequired)
        ));
        vault.locked.store(true, Ordering::Release);
        assert!(matches!(
            ensure_using(
                &owners,
                &state,
                request(&logical, &recipe, revision, false, false),
                &AtomicBool::new(false),
                |_| panic!("locked restore cannot load helper")
            ),
            Err(Error::Unavailable)
        ));
        assert_eq!(vault.interactive_reads.load(Ordering::SeqCst), reads);
        assert!(vault.noninteractive_reads.load(Ordering::SeqCst) > 0);
        assert_eq!(
            std::fs::read(state.join("owner-enrollment.json")).unwrap(),
            before
        );
        assert!(sessions
            .local_profile_target(
                &principal,
                &local_request(&request(&logical, &recipe, revision, false, false)),
                |_| Ok::<_, ControlError>(())
            )
            .unwrap()
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "owned local helper/PTY fixture only; FESTERM_HOST_TEST_HELPER required"]
    fn local_catalog_remote_off_disabled_host_create_reuse_unshare_and_restore_same_generation() {
        local_catalog_native_case(true);
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "owned local helper/PTY fixture only; FESTERM_HOST_TEST_HELPER required"]
    fn local_catalog_initially_unshared_remote_off_creates_only_explicit_local_generation() {
        local_catalog_native_case(false);
    }

    #[cfg(unix)]
    fn local_catalog_native_case(initially_enabled: bool) {
        use festerm_session::{noop_session_event_notifier, Session, SessionEvent};
        use std::time::{Duration, Instant};
        let (root, store, vault) = setup();
        let state = root.path().join("s");
        let owners = fixture_owners(&store, vault.clone());
        let helper = PathBuf::from(
            std::env::var_os("FESTERM_HOST_TEST_HELPER").expect("owned helper required"),
        );
        assert!(helper.is_absolute() && helper.is_file());
        let logical = LogicalSessionId::new();
        let recipe=Recipe{shell:Some("/bin/sh".into()),args:vec!["-c".into(),"while IFS= read -r line; do if [ \"$line\" = EXIT ]; then exit 0; fi; printf 'LOCAL:%s\\n' \"$line\"; done".into()],cwd:state.to_str().map(str::to_owned)};
        let sessions = SessionControl::with_helper(&state.join("daemon"), &helper).unwrap();
        let revision = sessions
            .set_profile(
                logical.clone(),
                "11111111-1111-4111-8111-111111111111",
                "Local",
                recipe.clone(),
                initially_enabled,
            )
            .unwrap();
        store.revoke().unwrap();
        let stopped = store.snapshot().unwrap();
        store
            .confirm_stopped(stopped.epoch, stopped.binding.as_ref().unwrap())
            .unwrap();
        let result = ensure_using(
            &owners,
            &state,
            request(&logical, &recipe, revision, initially_enabled, true),
            &AtomicBool::new(false),
            |_| Ok(helper.clone()),
        );
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                let retained = root.path().to_path_buf();
                std::mem::forget(root);
                panic!("local creation refused: {error}; owned fixture retained at {retained:?}");
            }
        };
        let target = result.summary.target.clone();
        let mut identity = result.identity.clone();
        // Daemon lease polling can briefly contend with these read-only lookups.
        // This fixture never retries creation, publication or uncertain outcomes.
        let restore = |recipe: &Recipe, revision, enabled| {
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                let result = ensure_using(
                    &owners,
                    &state,
                    request(&logical, recipe, revision, enabled, false),
                    &AtomicBool::new(false),
                    |_| panic!("existing-only lookup cannot resolve helper"),
                );
                match result {
                    Err(Error::Busy) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    result => break result,
                }
            }
        };
        let mut frontend = None;
        let body = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // An explicit Connect to the actual existing daemon must succeed even
            // when a new independent start would be blocked in this caller context.
            let running_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            let running = loop {
                let current = ensure_using_with_start_check(
                    &owners,
                    &state,
                    request(&logical, &recipe, revision, initially_enabled, true),
                    &AtomicBool::new(false),
                    |_| panic!("Running cannot resolve helper"),
                    || panic!("Running cannot query new-start context"),
                );
                match current {
                    Err(Error::Busy) if std::time::Instant::now() < running_deadline => {
                        std::thread::sleep(std::time::Duration::from_millis(5))
                    }
                    other => break other.unwrap(),
                }
            };
            assert_eq!(running.identity, result.identity);
            assert_eq!(running.summary.target, result.summary.target);
            assert_eq!(store.snapshot().unwrap().phase, Phase::Disabled);
            assert!(!store.snapshot().unwrap().settings.allow_creation);
            assert_eq!(target.logical, logical);
            let disabled = sessions
                .set_profile(
                    logical.clone(),
                    "11111111-1111-4111-8111-111111111111",
                    "Local",
                    recipe.clone(),
                    false,
                )
                .unwrap();
            let reused = restore(&recipe, disabled, false).unwrap();
            assert_eq!(reused.identity, identity);
            assert_eq!(reused.summary.target, target);
            let enabled_again = sessions
                .set_profile(
                    logical.clone(),
                    "11111111-1111-4111-8111-111111111111",
                    "Local",
                    recipe.clone(),
                    true,
                )
                .unwrap();
            let republished = restore(&recipe, enabled_again, true).unwrap();
            assert_eq!(republished.identity, identity);
            let disabled = sessions
                .set_profile(
                    logical.clone(),
                    "11111111-1111-4111-8111-111111111111",
                    "Local",
                    recipe.clone(),
                    false,
                )
                .unwrap();
            let principal = owners
                .local_session_policy_noninteractive(&state)
                .unwrap()
                .enrollment
                .binding
                .principal;
            assert!(sessions
                .inventory(&principal, |_| Ok::<_, ControlError>(()))
                .unwrap()
                .sessions
                .is_empty());
            let epoch = sessions
                .issuance_epoch(&principal, |_| Ok::<_, ControlError>(()))
                .unwrap();
            assert!(sessions
                .ensure_profile(
                    &principal,
                    &IssuedOperation {
                        id: OperationId::new(),
                        epoch
                    },
                    festerm_sessiond::host_control::ProfileEnsureRequest {
                        logical: logical.clone(),
                        revision: disabled,
                        attachment: festerm_host_protocol::AttachmentId::new()
                    },
                    &AtomicBool::new(false),
                    |_| Ok::<_, ControlError>(())
                )
                .is_err());
            let selected = festerm_sessiond::list_unattached_sessions_in(&result.runtime_root)
                .unwrap()
                .into_iter()
                .find(|entry| {
                    entry.pid == identity.pid
                        && entry.created_at_unix_ms == identity.created_at_unix_ms
                        && entry.endpoint == identity.endpoint
                })
                .unwrap();
            let session = festerm_sessiond::PersistentSession::resume_unattached_in(
                &selected,
                &result.runtime_root,
                noop_session_event_notifier(),
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while session.take_recovered_terminal().is_none() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            session.try_send_input(b"proof\n").unwrap();
            let mut output = Vec::new();
            while !output
                .windows(b"LOCAL:proof".len())
                .any(|window| window == b"LOCAL:proof")
            {
                if let Ok(SessionEvent::Output(bytes)) = session.try_recv_event() {
                    output.extend(bytes)
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            frontend = Some(session);
            let mut changed = recipe.clone();
            changed.args.push("changed".into());
            let changed_revision = sessions
                .set_profile(
                    logical.clone(),
                    "11111111-1111-4111-8111-111111111111",
                    "Local",
                    changed.clone(),
                    false,
                )
                .unwrap();
            assert!(matches!(
                restore(&changed, changed_revision, false),
                Err(Error::Stale)
            ));
            let old_identity = identity.clone();
            let session = frontend.as_ref().unwrap();
            session.try_send_input(b"EXIT\n").unwrap();
            let exit_deadline = Instant::now() + Duration::from_secs(5);
            while festerm_sessiond::list_running_sessions_strict_in(&result.runtime_root)
                .unwrap()
                .iter()
                .any(|entry| entry.session.pid == old_identity.pid)
            {
                assert!(
                    Instant::now() < exit_deadline,
                    "old exact generation did not finish"
                );
                while session.try_recv_event().is_ok() {}
                std::thread::sleep(Duration::from_millis(10));
            }
            drop(frontend.take());
            let recreated = ensure_using(
                &owners,
                &state,
                request(&logical, &changed, changed_revision, false, true),
                &AtomicBool::new(false),
                |_| Ok(helper.clone()),
            )
            .unwrap();
            identity = recreated.identity;
            assert_ne!(identity.created_at_unix_ms, old_identity.created_at_unix_ms);
            assert_eq!(recreated.summary.target.logical, logical);
            assert!(owners
                .local_session_policy_noninteractive(&state)
                .unwrap()
                .enrollment
                .keys
                .is_empty());
        }));
        if let Some(session) = frontend.take() {
            let _ = session.try_send_input(b"EXIT\n");
            drop(session);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while festerm_sessiond::list_running_sessions_strict_in(&result.runtime_root)
            .unwrap()
            .iter()
            .any(|entry| entry.session.pid == identity.pid)
        {
            if let Some(selected) =
                festerm_sessiond::list_unattached_sessions_in(&result.runtime_root)
                    .unwrap()
                    .into_iter()
                    .find(|entry| {
                        entry.pid == identity.pid
                            && entry.created_at_unix_ms == identity.created_at_unix_ms
                            && entry.endpoint == identity.endpoint
                    })
            {
                let exact = festerm_sessiond::terminate_unattached_session_command_in(
                    &selected,
                    &result.runtime_root,
                )
                .unwrap();
                let mut cleanup = std::process::Command::new(&helper)
                    .args(exact.get_args())
                    .env("FESTERM_SESSIOND_RUNTIME_ROOT", &result.runtime_root)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap();
                let cleanup_deadline = Instant::now() + Duration::from_secs(3);
                loop {
                    if let Some(status) = cleanup.try_wait().unwrap() {
                        assert!(status.success());
                        break;
                    }
                    if Instant::now() >= cleanup_deadline {
                        let _ = cleanup.kill();
                        let _ = cleanup.wait();
                        let retained = root.path().to_path_buf();
                        std::mem::forget(root);
                        panic!("owned cleanup command timed out; fixture retained at {retained:?}");
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            if Instant::now() >= deadline {
                let retained = root.path().to_path_buf();
                std::mem::forget(root);
                panic!("exact owned cleanup did not finish; fixture retained at {retained:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if let Err(panic) = body {
            std::panic::resume_unwind(panic)
        }
    }
}
