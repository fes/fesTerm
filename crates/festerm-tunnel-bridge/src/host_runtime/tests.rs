use super::*;
use crate::{
    host_control::tests::Policy,
    owner_auth::{fixture_key, OwnerPolicy, OwnerVerifier, PinnedHost},
    relay_host::tests::owner_network_pair,
};
use festerm_host_protocol::{
    Capabilities, Envelope, Epoch, OperationId, PrincipalId, Request, Response,
};
use festerm_secret_store::MemorySecretStore;
use festerm_tunnel_client::host_control::{Binding, ControlClient};
use russh::keys::{Algorithm, HashAlg, PrivateKey};
use std::path::PathBuf;

struct Fixture {
    runtime: Arc<HostRuntime<Policy>>,
    policy: OwnerPolicy,
    source: Arc<Policy>,
    approved: OwnerBinding,
    key: Arc<ClientOwnerKey>,
    vault: Arc<MemorySecretStore>,
    ssh_key: PrivateKey,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let ssh_key =
            PrivateKey::random(&mut russh::keys::key::safe_rng(), Algorithm::Ed25519).unwrap();
        let host = PinnedHost {
            host: Default::default(),
            host_key_fingerprint: ssh_key
                .public_key()
                .fingerprint(HashAlg::Sha256)
                .to_string(),
        };
        let approved = OwnerBinding {
            installation: festerm_host_protocol::HostId::new().to_string(),
            host: host.clone(),
            account: "github:runtime-test".into(),
            principal: PrincipalId::new(),
        };
        let vault = Arc::new(MemorySecretStore::new());
        let key = Arc::new(fixture_key(vault.as_ref(), &approved));
        let policy = OwnerPolicy {
            host,
            principal: approved.principal.clone(),
            host_epoch: Epoch::new(1).unwrap(),
            grant_epoch: Epoch::new(1).unwrap(),
            remote_access: true,
            capabilities: Capabilities {
                inventory: true,
                attach: true,
                detach: true,
                ..Capabilities::default()
            },
        };
        let source = Arc::new(Policy(Arc::new(tokio::sync::Mutex::new(
            OwnerVerifier::from_protected_permissions(policy.clone(), &[key.public_key()]).unwrap(),
        ))));
        let root = crate::test_support::private_directory("host-runtime");
        // These tests never launch a process. A real helper is required by the
        // separate opt-in owned-daemon terminal cases.
        let sessions =
            SessionControl::with_helper(&root, &std::env::current_exe().unwrap()).unwrap();
        Self {
            runtime: Arc::new(HostRuntime::new(sessions, source.clone())),
            policy,
            source,
            approved,
            key,
            vault,
            ssh_key,
            root,
        }
    }

    async fn pair(&self) -> (ClientConnection, HostStream, tokio::task::JoinHandle<()>) {
        owner_network_pair(self.ssh_key.clone(), self.approved.host.clone()).await
    }

    async fn authenticate(
        &self,
        client: ClientConnection,
        role: StreamRole,
    ) -> (ClientConnection, ConnectionPermit) {
        let permit = ConnectionPermit::reserve(&Budgets::default()).unwrap();
        let vault = self.vault.clone();
        let key = self.key.clone();
        let approved = self.approved.clone();
        authenticate_client_using(
            client,
            permit,
            role,
            CancellationToken::new(),
            move |stream, challenge| {
                stream.prove_owner_fixture(vault.as_ref(), &key, &approved, challenge)
            },
        )
        .await
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

struct BusyPolicy {
    inner: Arc<Policy>,
    remaining: std::sync::atomic::AtomicUsize,
    simulated_busy: std::sync::atomic::AtomicUsize,
    locked_busy: std::sync::atomic::AtomicUsize,
}

impl ProtectedPolicySource for BusyPolicy {
    type Guard = <Policy as ProtectedPolicySource>::Guard;

    fn current_policy(&self) -> Result<Self::Guard, ContractError> {
        use std::sync::atomic::Ordering::SeqCst;
        let mut remaining = self.remaining.load(SeqCst);
        while let Some(next) = remaining.checked_sub(1) {
            match self
                .remaining
                .compare_exchange_weak(remaining, next, SeqCst, SeqCst)
            {
                Ok(_) => {
                    self.simulated_busy.fetch_add(1, SeqCst);
                    return Err(ContractError::Busy);
                }
                Err(current) => remaining = current,
            }
        }
        let current = self.inner.current_policy();
        if matches!(current, Err(ContractError::Busy)) {
            self.locked_busy.fetch_add(1, SeqCst);
        }
        current
    }
}

fn busy_runtime(
    fixture: &Fixture,
    remaining: usize,
) -> (Arc<HostRuntime<BusyPolicy>>, Arc<BusyPolicy>) {
    let source = Arc::new(BusyPolicy {
        inner: fixture.source.clone(),
        remaining: remaining.into(),
        simulated_busy: 0.into(),
        locked_busy: 0.into(),
    });
    let sessions =
        SessionControl::with_helper(&fixture.root, &std::env::current_exe().unwrap()).unwrap();
    (Arc::new(HostRuntime::new(sessions, source.clone())), source)
}

async fn wait_for_busy(counter: &std::sync::atomic::AtomicUsize, minimum: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while counter.load(std::sync::atomic::Ordering::SeqCst) < minimum {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("authentication did not reach protected-policy contention");
}

fn client_budget_drained(runtime: &HostRuntime<BusyPolicy>) -> bool {
    let permits: Result<Vec<_>, _> = (0..MAX_CLIENTS)
        .map(|_| {
            runtime
                .budgets
                .reserve(festerm_host_protocol::Resource::Client, 1)
        })
        .collect();
    permits.is_ok()
}

#[tokio::test]
async fn authenticated_runtime_policy_busy_retries_both_phases_and_rechecks_actual_guard() {
    use std::sync::atomic::Ordering::SeqCst;
    for revoke in [false, true] {
        let fixture = Fixture::new();
        let (runtime, source) = busy_runtime(&fixture, 3);
        let permit = ConnectionPermit::reserve(&Budgets::default()).unwrap();
        let held = fixture.source.0.clone().lock_owned().await;
        let (mut client, stream, ssh) = fixture.pair().await;
        let host = runtime.clone();
        let server =
            tokio::spawn(async move { host.serve_stream(stream, CancellationToken::new()).await });
        wait_for_busy(&source.locked_busy, 1).await;
        drop(held);
        let frame = read_frame(&mut client, permit.budgets()).await.unwrap();
        let Authentication::Challenge { challenge } = frame.decode().unwrap() else {
            panic!("contention must not replace the original SSH challenge");
        };
        drop(frame);
        let proof = client
            .prove_owner_fixture(
                fixture.vault.as_ref(),
                &fixture.key,
                &fixture.approved,
                &challenge,
            )
            .unwrap();
        let mut held = fixture.source.0.clone().lock_owned().await;
        source.remaining.store(3, SeqCst);
        write_frame(
            &mut client,
            permit.budgets(),
            &Authentication::Proof { proof },
        )
        .await
        .unwrap();
        wait_for_busy(&source.locked_busy, 2).await;
        if revoke {
            let mut changed = fixture.policy.clone();
            changed.remote_access = false;
            *held = OwnerVerifier::from_protected_permissions(changed, &[fixture.key.public_key()])
                .unwrap();
        }
        drop(held);
        let frame = read_frame(&mut client, permit.budgets()).await.unwrap();
        let response: Authentication = frame.decode().unwrap();
        drop(frame);
        if revoke {
            assert!(matches!(response, Authentication::Denied {}));
        } else {
            assert!(matches!(response, Authentication::Accepted {}));
            client.write_u8(0).await.unwrap();
            client.flush().await.unwrap();
        }
        drop(client);
        drop(permit);
        let result = tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
        if revoke {
            assert!(matches!(
                result,
                Err(Error::Contract(ContractError::Denied))
            ));
        }
        assert!(source.simulated_busy.load(SeqCst) >= 6);
        assert!(client_budget_drained(&runtime));
        ssh.abort();
        let _ = ssh.await;
    }
}

#[tokio::test]
async fn authenticated_runtime_policy_busy_refusal_uses_one_deadline_for_both_phases() {
    use std::sync::atomic::Ordering::SeqCst;
    for after_challenge in [false, true] {
        let fixture = Fixture::new();
        let (runtime, source) =
            busy_runtime(&fixture, if after_challenge { 0 } else { usize::MAX });
        let permit = ConnectionPermit::reserve(&Budgets::default()).unwrap();
        let (client, stream, ssh) = fixture.pair().await;
        let host = runtime.clone();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        let host_permit = Arc::new(ConnectionPermit::reserve(&host.budgets).unwrap());
        let server = tokio::spawn(async move {
            tokio::time::timeout_at(
                deadline,
                host.authenticate(stream, host_permit, deadline, CancellationToken::new()),
            )
            .await
            .expect("policy contention restarted or exhausted the authentication deadline")
        });
        let vault = fixture.vault.clone();
        let key = fixture.key.clone();
        let approved = fixture.approved.clone();
        let result = authenticate_client_using(
            client,
            permit,
            StreamRole::Control,
            CancellationToken::new(),
            move |stream, challenge| {
                if after_challenge {
                    std::thread::sleep(Duration::from_millis(200));
                    source.remaining.store(usize::MAX, SeqCst);
                }
                stream.prove_owner_fixture(vault.as_ref(), &key, &approved, challenge)
            },
        )
        .await;
        assert!(matches!(
            result,
            Err(Error::Refused(festerm_host_protocol::Refusal::Busy))
        ));
        assert!(matches!(
            server.await.unwrap(),
            Err(Error::Refused(festerm_host_protocol::Refusal::Busy))
        ));
        assert!(client_budget_drained(&runtime));
        ssh.abort();
        let _ = ssh.await;
    }
}

#[tokio::test]
async fn authenticated_runtime_policy_persistent_busy_is_framed_before_real_auth_deadline() {
    let fixture = Fixture::new();
    let (runtime, source) = busy_runtime(&fixture, usize::MAX);
    let permit = ConnectionPermit::reserve(&Budgets::default()).unwrap();
    let (client, stream, ssh) = fixture.pair().await;
    let host = runtime.clone();
    let started = tokio::time::Instant::now();
    let server =
        tokio::spawn(async move { host.serve_stream(stream, CancellationToken::new()).await });
    let vault = fixture.vault.clone();
    let key = fixture.key.clone();
    let approved = fixture.approved.clone();
    let result = authenticate_client_using(
        client,
        permit,
        StreamRole::Control,
        CancellationToken::new(),
        move |stream, challenge| {
            stream.prove_owner_fixture(vault.as_ref(), &key, &approved, challenge)
        },
    )
    .await;
    assert!(matches!(
        result,
        Err(Error::Refused(festerm_host_protocol::Refusal::Busy))
    ));
    assert!(matches!(
        server.await.unwrap(),
        Err(Error::Refused(festerm_host_protocol::Refusal::Busy))
    ));
    assert!(started.elapsed() >= AUTH_DEADLINE - AUTH_REFUSAL_RESERVE);
    assert!(started.elapsed() < AUTH_DEADLINE + Duration::from_secs(1));
    assert!(
        source
            .simulated_busy
            .load(std::sync::atomic::Ordering::SeqCst)
            > 1
    );
    assert!(client_budget_drained(&runtime));
    ssh.abort();
    let _ = ssh.await;
}

#[tokio::test]
async fn authenticated_runtime_policy_busy_cancellation_drains_charged_work() {
    let fixture = Fixture::new();
    let (runtime, source) = busy_runtime(&fixture, usize::MAX);
    let permit = ConnectionPermit::reserve(&Budgets::default()).unwrap();
    let (client, stream, ssh) = fixture.pair().await;
    let cancel = CancellationToken::new();
    let host = runtime.clone();
    let host_cancel = cancel.clone();
    let server = tokio::spawn(async move { host.serve_stream(stream, host_cancel).await });
    let vault = fixture.vault.clone();
    let key = fixture.key.clone();
    let approved = fixture.approved.clone();
    let client_cancel = cancel.clone();
    let client = tokio::spawn(authenticate_client_using(
        client,
        permit,
        StreamRole::Control,
        client_cancel,
        move |stream, challenge| {
            stream.prove_owner_fixture(vault.as_ref(), &key, &approved, challenge)
        },
    ));
    wait_for_busy(&source.simulated_busy, 1).await;
    cancel.cancel();
    assert!(matches!(client.await.unwrap(), Err(Error::Cancelled)));
    assert!(matches!(server.await.unwrap(), Err(Error::Cancelled)));
    tokio::time::timeout(Duration::from_secs(2), async {
        while !client_budget_drained(&runtime) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("cancelled Busy work retained an unbounded client charge");
    ssh.abort();
    let _ = ssh.await;
}

#[cfg(unix)]
struct NativeRuntimeFixture {
    auth: Fixture,
    native: crate::host_control::tests::native_routes::NativeFixture,
    runtime: Arc<HostRuntime<Policy>>,
    servers: Vec<tokio::task::JoinHandle<Result<(), Error>>>,
    connections: Vec<tokio::task::JoinHandle<()>>,
}

#[cfg(unix)]
impl NativeRuntimeFixture {
    fn client_context(&self) -> crate::host_client::ApprovedContext {
        let public = self.auth.ssh_key.public_key().to_openssh().unwrap();
        crate::host_client::ApprovedContext::from_protected_approval(
            self.auth.approved.clone(),
            festerm_secret_store::SecretReference::generate(),
            serde_json::from_value(serde_json::json!({
                "connectionMode": "TunnelRelay", "hostId": "owned-native-runtime",
                "hostPublicKeys": [public.split_whitespace().nth(1).unwrap()],
                "clientRelayUri": "wss://example.invalid/client"
            }))
            .unwrap(),
            22,
            zeroize::Zeroizing::new("fixture-token".into()),
            self.binding(),
        )
        .unwrap()
    }

    fn new() -> Self {
        let mut auth = Fixture::new();
        let mut native = crate::host_control::tests::native_routes::NativeFixture::new(true);
        auth.policy.capabilities = Capabilities {
            inventory: true,
            ensure: true,
            create: true,
            attach: true,
            detach: true,
            explicit_end: true,
            temporary: true,
            creation_allowed: true,
            ..Capabilities::default()
        };
        *auth.source.0.try_lock().unwrap() = OwnerVerifier::from_protected_permissions(
            auth.policy.clone(),
            &[auth.key.public_key()],
        )
        .unwrap();
        native.policy = auth.policy.clone();
        let runtime = Arc::new(HostRuntime::new(native.control(), auth.source.clone()));
        Self {
            auth,
            native,
            runtime,
            servers: Vec::new(),
            connections: Vec::new(),
        }
    }
}

#[cfg(unix)]
async fn client_worker_result(
    worker: &crate::host_client::Worker,
    scope: &crate::host_client::Scope,
) -> crate::host_client::Outcome {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(completion) = worker.try_receive(scope) {
                assert_eq!(&completion.scope, scope);
                return completion.outcome;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("bounded actual app worker completion")
}

#[cfg(unix)]
async fn client_create_worker(
    fixture: &mut NativeRuntimeFixture,
    intent: Arc<crate::host_client::CreationIntent>,
) -> crate::host_client::Created {
    tokio::time::timeout(Duration::from_secs(8), async {
        let mut original = intent.issued_operation();
        loop {
            let (stream, permit) = fixture.stream(StreamRole::Control).await;
            let scope = crate::host_client::Scope {
                binding: fixture.binding(),
                request: OperationId::new(),
                generation: 1,
            };
            let binding = scope.binding.clone();
            let action = intent.clone();
            let worker = crate::host_client::Worker::spawn(
                scope.clone(),
                Arc::new(|| {}),
                move |_, cancel, _, _| async move {
                    let mut client =
                        ControlClient::on_authenticated_stream(stream, binding, permit);
                    crate::host_client::create_authenticated(&mut client, action, &cancel).await
                },
            )
            .unwrap();
            let result = client_worker_result(&worker, &scope).await;
            let issued = intent.issued_operation();
            if let Some(expected) = &original {
                assert_eq!(
                    issued.as_ref(),
                    Some(expected),
                    "fixture retry retains original issuance"
                );
            } else {
                original = issued;
            }
            match result {
                crate::host_client::Outcome::Created(created) => return *created,
                crate::host_client::Outcome::Failed(crate::host_client::Failure::Control(
                    festerm_tunnel_client::host_control::Error::Refused(
                        festerm_host_protocol::Refusal::Busy,
                    ),
                )) => {
                    drop(worker);
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                crate::host_client::Outcome::Failed(error) => {
                    panic!("actual creation worker: {error:?}")
                }
                _ => {
                    panic!("actual app creation worker must return a correlated native generation")
                }
            }
        }
    })
    .await
    .expect("same original bounded native creation fixture deadline")
}

#[cfg(unix)]
async fn client_end_worker(
    fixture: &mut NativeRuntimeFixture,
    intent: Arc<crate::host_client::EndIntent>,
) -> crate::host_client::Outcome {
    let (stream, permit) = fixture.stream(StreamRole::Control).await;
    let scope = crate::host_client::Scope {
        binding: fixture.binding(),
        request: OperationId::new(),
        generation: 1,
    };
    let binding = scope.binding.clone();
    let worker = crate::host_client::Worker::spawn(
        scope.clone(),
        Arc::new(|| {}),
        move |_, cancel, _, _| async move {
            let mut client = ControlClient::on_authenticated_stream(stream, binding, permit);
            crate::host_client::end_authenticated(&mut client, intent, &cancel).await
        },
    )
    .unwrap();
    client_worker_result(&worker, &scope).await
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires explicitly built owned native helper and fixture child"]
async fn native_mobile_host_retirement_keeps_persistent_and_ends_only_owning_temporary() {
    use crate::host_client::{CreationIntent, CreationMode, HostSession, Scope, Worker};
    use festerm_host_protocol::Lifetime;
    use festerm_session::{
        Session, SessionEvent, SessionEventNotifier, SessionLifecycle, ShutdownResult,
    };
    struct Wake;
    impl SessionEventNotifier for Wake {
        fn notify(&self) {}
    }
    let mut fixture = NativeRuntimeFixture::new();
    let context = fixture.client_context();
    let mut persistent_target = None;
    for (temporary, label) in [(false, "PERSISTENT"), (true, "TEMPORARY")] {
        let mut creation = fixture.native.creation();
        if temporary {
            creation.lifetime = Lifetime::Temporary {
                reconnect_grace_seconds: 60,
            };
        }
        creation.recipe.args = vec![
            format!("emit:{label}-READY"),
            "read-line".into(),
            format!("echo:{label}"),
            "spin".into(),
        ];
        let intent =
            Arc::new(CreationIntent::new(&context, CreationMode::Create, creation).unwrap());
        let created = client_create_worker(&mut fixture, intent.clone()).await;
        let target = created.session().target.clone();
        let (stream, permit) = fixture.stream(StreamRole::Terminal).await;
        let scope = Scope {
            request: OperationId::new(),
            generation: 1,
            binding: fixture.binding(),
        };
        let selection = (target.clone(), intent.creation().attachment.clone());
        let worker = Worker::spawn(
            scope,
            Arc::new(|| {}),
            move |scope, cancel, events, wake| async move {
                crate::host_client::attach_reserved_authenticated(
                    stream, permit, &scope, selection, &cancel, &events, &wake,
                )
                .await
            },
        )
        .unwrap();
        let session = Arc::new(HostSession::from_worker(worker, Arc::new(Wake)));
        tokio::time::timeout(Duration::from_secs(8), async {
            while !session.has_pending_snapshot() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let mut frontend =
            festerm_core::Terminal::new(festerm_core::Dimensions::new(40, 12).unwrap()).unwrap();
        assert!(session.adopt_into(&mut frontend).unwrap());
        tokio::time::timeout(Duration::from_secs(8), async {
            while session.lifecycle() != SessionLifecycle::Running {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        session.try_send_input(b"once\n").unwrap();
        let expected = format!("{label}:once");
        let foreign = if temporary {
            "PERSISTENT:once"
        } else {
            "TEMPORARY:once"
        };
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Ok(SessionEvent::Output(bytes)) = session.try_recv_event() {
                    frontend.ingest(&bytes);
                }
                assert!(!frontend.text_snapshot().text().contains(foreign));
                if frontend.text_snapshot().text().contains(&expected) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let retiring = session.clone();
        assert_eq!(
            tokio::task::spawn_blocking(move || retiring.shutdown(Duration::from_secs(3)))
                .await
                .unwrap()
                .unwrap(),
            ShutdownResult::Stopped
        );
        assert_eq!(session.lifecycle(), SessionLifecycle::Stopped);
        assert!(session.try_send_input(b"never-replay").is_err());
        let lease = fixture.lease(&target);
        assert_eq!(lease["connected"], false);
        assert_eq!(lease["stopped"], temporary);
        if temporary {
            tokio::time::timeout(Duration::from_secs(8), async {
                while festerm_sessiond::list_running_sessions_in(&fixture.native.root)
                    .unwrap()
                    .len()
                    != 1
                {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .unwrap();
        } else {
            assert_eq!(
                festerm_sessiond::list_running_sessions_in(&fixture.native.root)
                    .unwrap()
                    .len(),
                1
            );
            persistent_target = Some(target.clone());
        }
        drop(session);
    }
    let target = persistent_target.unwrap();
    let mut pending = fixture
        .pending(
            target.clone(),
            festerm_host_protocol::AttachmentId::new(),
            false,
        )
        .await;
    let recovered = pending.take_snapshot().unwrap();
    assert!(recovered.text_snapshot().text().contains("PERSISTENT:once"));
    assert!(!recovered.text_snapshot().text().contains("TEMPORARY:once"));
    let terminal = pending
        .adopt(&CancellationToken::new())
        .await
        .unwrap()
        .spawn()
        .unwrap();
    drop(terminal);
    assert!(
        !fixture.lease(&target)["stopped"].as_bool().unwrap(),
        "client disappearance is not persistent End"
    );
    fixture.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires explicitly built owned native helper and fixture child"]
async fn native_selected_host_creation_worker_adopts_isolated_frontends_and_original_ensure() {
    use crate::host_client::{CreationIntent, CreationMode, Outcome, Scope, Worker};
    use festerm_host_protocol::EnsureDisposition;
    let mut fixture = NativeRuntimeFixture::new();
    let context = fixture.client_context();
    let mut intents = Vec::new();
    let mut targets = Vec::new();
    for label in ["A", "B"] {
        let mut creation = fixture.native.creation();
        creation.recipe.args = vec![
            format!("emit:ready-{label}"),
            "read-line".into(),
            format!("echo:{label}"),
            "spin".into(),
        ];
        let mode = if label == "A" {
            CreationMode::Create
        } else {
            CreationMode::Ensure
        };
        let intent = Arc::new(CreationIntent::new(&context, mode, creation).unwrap());
        let created = client_create_worker(&mut fixture, intent.clone()).await;
        assert_eq!(created.disposition(), EnsureDisposition::Created);
        assert_eq!(created.session().target.logical, intent.creation().logical);
        targets.push(created.session().target.clone());
        intents.push(intent);
    }
    for (intent, target) in intents.iter().zip(&targets) {
        let issued = intent.issued_operation().unwrap();
        let retry = client_create_worker(&mut fixture, intent.clone()).await;
        assert_eq!(&retry.session().target, target);
        assert_eq!(retry.disposition(), EnsureDisposition::Created);
        assert_eq!(intent.issued_operation(), Some(issued));
    }
    let ensure = Arc::new(
        CreationIntent::new(
            &context,
            CreationMode::Ensure,
            intents[0].creation().clone(),
        )
        .unwrap(),
    );
    let resumed = client_create_worker(&mut fixture, ensure).await;
    assert_eq!(resumed.disposition(), EnsureDisposition::Resumed);
    assert_eq!(resumed.session().target, targets[0]);
    assert_eq!(
        festerm_sessiond::list_running_sessions_in(&fixture.native.root)
            .unwrap()
            .len(),
        2
    );

    let mut workers = Vec::new();
    let mut live = Vec::new();
    let mut frontends = Vec::new();
    for (target, intent) in targets.iter().cloned().zip(&intents) {
        let (stream, permit) = fixture.stream(StreamRole::Terminal).await;
        let scope = Scope {
            binding: fixture.binding(),
            request: OperationId::new(),
            generation: 2,
        };
        let selection = (target, intent.creation().attachment.clone());
        let worker = Worker::spawn(
            scope.clone(),
            Arc::new(|| {}),
            move |scope, cancel, events, wake| async move {
                crate::host_client::attach_reserved_authenticated(
                    stream, permit, &scope, selection, &cancel, &events, &wake,
                )
                .await
            },
        )
        .unwrap();
        let Outcome::Snapshot(adoption) = client_worker_result(&worker, &scope).await else {
            panic!("real frontend snapshot must precede activation/input")
        };
        assert!(worker.try_receive(&scope).is_none());
        let mut frontend =
            festerm_core::Terminal::new(festerm_core::Dimensions::new(80, 24).unwrap()).unwrap();
        adoption.adopt_into(&scope, &mut frontend).unwrap();
        let Outcome::Attached(handle) = client_worker_result(&worker, &scope).await else {
            panic!("host must independently confirm frontend activation")
        };
        live.push(handle);
        workers.push(worker);
        frontends.push(frontend);
    }
    live[0].input(b"alpha\n").unwrap();
    live[1].input(b"beta\n").unwrap();
    for (index, expected, foreign) in [(0, "A:alpha", "B:beta"), (1, "B:beta", "A:alpha")] {
        let mut observed = Vec::new();
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let frame = live[index].next_event().await.unwrap();
                if frame.kind() == crate::protocol::OUTPUT {
                    frontends[index].ingest(frame.payload());
                    observed.extend_from_slice(frame.payload());
                    assert!(observed.len() < 64 * 1024);
                    assert!(!observed
                        .windows(foreign.len())
                        .any(|bytes| bytes == foreign.as_bytes()));
                    if observed
                        .windows(expected.len())
                        .any(|bytes| bytes == expected.as_bytes())
                    {
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();
        let text = frontends[index].text_snapshot();
        assert!(
            text.text().contains(expected),
            "actual presented frontend must contain its native child output"
        );
        assert!(!text.text().contains(foreign));
    }
    for (handle, target) in live.iter_mut().zip(&targets) {
        let operation = handle.request_detach().unwrap();
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let frame = handle.next_event().await.unwrap();
                if frame.kind() == crate::host_terminal::HOST_REPLY {
                    let reply: Envelope<Response> =
                        festerm_host_protocol::decode(frame.payload()).unwrap();
                    assert_eq!(reply.operation, operation);
                    assert!(matches!(reply.body, Response::Detached { target: actual } if actual == *target));
                    break;
                }
            }
        })
        .await
        .unwrap();
    }
    drop(live);
    drop(workers);
    assert_eq!(
        festerm_sessiond::list_running_sessions_in(&fixture.native.root)
            .unwrap()
            .len(),
        2
    );
    fixture.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires explicitly built owned native helper and fixture child"]
async fn native_mobile_host_session_adopts_actual_pty_and_retains_original_until_activation() {
    use crate::host_client::{CreationIntent, CreationMode, HostSession, Scope, Worker};
    use festerm_session::{Session, SessionEvent, SessionEventNotifier, SessionLifecycle};
    struct Wake;
    impl SessionEventNotifier for Wake {
        fn notify(&self) {}
    }
    let mut fixture = NativeRuntimeFixture::new();
    let context = fixture.client_context();
    let mut creation = fixture.native.creation();
    creation.recipe.args = vec![
        "emit:OWNED-MOBILE".into(),
        "read-line".into(),
        "echo:MOBILE".into(),
        "spin".into(),
    ];
    let intent = Arc::new(CreationIntent::new(&context, CreationMode::Ensure, creation).unwrap());
    let created = client_create_worker(&mut fixture, intent.clone()).await;
    let target = created.session().target.clone();
    let issued = intent.issued_operation().unwrap();
    assert!(!intent.is_presented());
    let mut foreign_creation = fixture.native.creation();
    foreign_creation.recipe.args = vec![
        "read-line".into(),
        "echo:FOREIGN-MOBILE".into(),
        "spin".into(),
    ];
    let foreign_intent =
        Arc::new(CreationIntent::new(&context, CreationMode::Create, foreign_creation).unwrap());
    let foreign_created = client_create_worker(&mut fixture, foreign_intent.clone()).await;
    let foreign_target = foreign_created.session().target.clone();
    assert_ne!(target, foreign_target);
    let mut foreign_pending = fixture
        .pending(
            foreign_target.clone(),
            foreign_intent.creation().attachment.clone(),
            false,
        )
        .await;
    let mut foreign_frontend = foreign_pending.take_snapshot().unwrap();
    let mut foreign = foreign_pending
        .adopt(&CancellationToken::new())
        .await
        .unwrap()
        .spawn()
        .unwrap();
    foreign.input(b"independent\n").unwrap();
    let foreign_output = terminal_output_until(&mut foreign, b"FOREIGN-MOBILE:independent").await;
    foreign_frontend.ingest(&foreign_output);
    assert!(foreign_frontend
        .text_snapshot()
        .text()
        .contains("FOREIGN-MOBILE:independent"));
    assert!(!foreign_frontend
        .text_snapshot()
        .text()
        .contains("OWNED-MOBILE"));
    let (stream, permit) = fixture.stream(StreamRole::Terminal).await;
    let scope = Scope {
        request: OperationId::new(),
        generation: 1,
        binding: fixture.binding(),
    };
    let selection = (target.clone(), intent.creation().attachment.clone());
    let worker = Worker::spawn(
        scope,
        Arc::new(|| {}),
        move |scope, cancel, events, wake| async move {
            crate::host_client::attach_reserved_authenticated(
                stream, permit, &scope, selection, &cancel, &events, &wake,
            )
            .await
        },
    )
    .unwrap();
    let mut session = HostSession::from_worker(worker, Arc::new(Wake));
    session.set_creation_intent(Some(intent.clone()));
    tokio::time::timeout(Duration::from_secs(8), async {
        while !session.has_pending_snapshot() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(session.lifecycle(), SessionLifecycle::Starting);
    assert!(
        !intent.is_presented(),
        "starting a worker is not frontend presentation"
    );
    assert!(session.try_send_input(b"must-not-be-queued").is_err());
    let mut frontend =
        festerm_core::Terminal::new(festerm_core::Dimensions::new(40, 12).unwrap()).unwrap();
    assert!(session.adopt_into(&mut frontend).unwrap());
    tokio::time::timeout(Duration::from_secs(8), async {
        while session.lifecycle() != SessionLifecycle::Running {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(intent.is_presented());
    assert_eq!(intent.issued_operation(), Some(issued));
    session.try_send_input(b"once\n").unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Ok(SessionEvent::Output(bytes)) = session.try_recv_event() {
                frontend.ingest(&bytes);
            }
            let text = frontend.text_snapshot();
            assert!(!text.text().contains("FOREIGN-MOBILE"));
            if text.text().contains("MOBILE:once") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(frontend.text_snapshot().text().contains("OWNED-MOBILE"));
    session.cancel();
    assert!(session.try_send_input(b"never-replay").is_err());
    drop(session);
    let mut control = fixture.control().await;
    let operation = OperationId::new();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let response = control
                .request(
                    operation.clone(),
                    Request::List { cursor: None },
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
            match response.body {
                Response::Refused {
                    reason: festerm_host_protocol::Refusal::Busy,
                } => tokio::time::sleep(Duration::from_millis(25)).await,
                Response::Inventory { page } => {
                    assert!(page.sessions.iter().any(|session| session.target == target));
                    assert!(page
                        .sessions
                        .iter()
                        .any(|session| session.target == foreign_target));
                    break;
                }
                other => panic!("cancel is not detach, end, or absence: {other:?}"),
            }
        }
    })
    .await
    .unwrap();
    drop(control);
    drop(foreign);
    fixture.shutdown().await;
}

#[cfg(unix)]
impl NativeRuntimeFixture {
    fn binding(&self) -> Binding {
        Binding::new(
            self.auth.approved.host.host.clone(),
            self.auth.approved.account.clone(),
            "owned-native-runtime".into(),
        )
        .unwrap()
    }

    async fn stream(&mut self, role: StreamRole) -> (ClientConnection, ConnectionPermit) {
        let (client, stream, ssh) = self.auth.pair().await;
        let runtime = self.runtime.clone();
        self.servers.push(tokio::spawn(async move {
            runtime.serve_stream(stream, CancellationToken::new()).await
        }));
        self.connections.push(ssh);
        self.auth.authenticate(client, role).await
    }

    async fn control(&mut self) -> ControlClient<ClientConnection> {
        let (client, permit) = self.stream(StreamRole::Control).await;
        let mut control = ControlClient::on_authenticated_stream(client, self.binding(), permit);
        let capabilities = control.handshake(&CancellationToken::new()).await.unwrap();
        assert!(capabilities.ensure && capabilities.create && capabilities.explicit_end);
        assert!(capabilities.attach && capabilities.detach && capabilities.temporary);
        assert!(capabilities.issuance_epoch.is_some());
        control
    }

    async fn pending(
        &mut self,
        target: festerm_host_protocol::Target,
        attachment: festerm_host_protocol::AttachmentId,
        takeover: bool,
    ) -> crate::host_terminal::PendingTerminal<ClientConnection> {
        let (client, permit) = self.stream(StreamRole::Terminal).await;
        crate::host_terminal::establish(
            client,
            permit,
            self.binding(),
            OperationId::new(),
            target,
            attachment,
            takeover,
            &CancellationToken::new(),
        )
        .await
        .unwrap()
    }

    fn lease(&self, target: &festerm_host_protocol::Target) -> serde_json::Value {
        // Observation of this exact owned fixture only, never runtime authority.
        serde_json::from_slice(
            &std::fs::read(
                self.native
                    .root
                    .join(format!("host-daemon-{}.json", target.generation.get())),
            )
            .unwrap(),
        )
        .unwrap()
    }

    async fn shutdown(self) {
        for server in self.servers {
            let _ = tokio::time::timeout(Duration::from_secs(8), server)
                .await
                .expect("owned runtime stream did not close")
                .expect("owned runtime stream panicked");
        }
        for ssh in self.connections {
            ssh.abort();
            let _ = ssh.await;
        }
    }
}

#[cfg(unix)]
async fn issued_request(
    control: &mut ControlClient<ClientConnection>,
    issued: &festerm_tunnel_client::host_control::IssuedControlOperation,
    request: Request,
) -> Response {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let response = control
                .request_issued(issued, request.clone(), &CancellationToken::new())
                .await
                .unwrap()
                .body;
            if matches!(
                response,
                Response::Refused {
                    reason: festerm_host_protocol::Refusal::Busy
                }
            ) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            } else {
                break response;
            }
        }
    })
    .await
    .unwrap()
}

#[cfg(unix)]
async fn create_remote(
    control: &mut ControlClient<ClientConnection>,
    creation: festerm_host_protocol::Creation,
) -> festerm_host_protocol::Target {
    let issued = control.issue_operation().unwrap();
    match issued_request(control, &issued, Request::Create { creation }).await {
        Response::Ensured { session, .. } => session.target,
        response => panic!("remote create failed: {response:?}"),
    }
}

#[cfg(unix)]
async fn terminal_output_until(
    terminal: &mut crate::host_terminal::TerminalHandle,
    expected: &[u8],
) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(8), async {
        let mut output = Vec::new();
        loop {
            let frame = terminal.next_event().await.unwrap();
            if frame.kind() == crate::protocol::OUTPUT {
                output.extend_from_slice(frame.payload());
                assert!(output.len() <= festerm_host_protocol::MAX_BYTES_PER_CLIENT);
                if output
                    .windows(expected.len())
                    .any(|bytes| bytes == expected)
                {
                    break output;
                }
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn selected_host_signed_ssh_inventory_uses_the_authenticated_control_stream() {
    let fixture = Fixture::new();
    let (client, stream, ssh) = fixture.pair().await;
    let runtime = fixture.runtime.clone();
    let server =
        tokio::spawn(async move { runtime.serve_stream(stream, CancellationToken::new()).await });
    let (client, permit) = fixture.authenticate(client, StreamRole::Control).await;
    let binding = Binding::new(
        fixture.approved.host.host.clone(),
        fixture.approved.account.clone(),
        "selected-host-fixture".into(),
    )
    .unwrap();
    let mut client = ControlClient::on_authenticated_stream(client, binding, permit);
    let inventory = crate::host_client::inventory_authenticated(
        &mut client,
        OperationId::new(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(inventory.capabilities.inventory);
    assert!(!inventory.capabilities.create && !inventory.capabilities.ensure);
    assert!(inventory.sessions.is_empty() && inventory.unknown.is_empty());
    drop(client);
    assert!(server.await.unwrap().is_err());
    ssh.abort();
    let _ = ssh.await;
}

#[tokio::test]
async fn authenticated_runtime_routes_actual_ssh_control_and_rechecks_revocation() {
    let fixture = Fixture::new();
    let (client, stream, ssh) = fixture.pair().await;
    let runtime = fixture.runtime.clone();
    let server =
        tokio::spawn(async move { runtime.serve_stream(stream, CancellationToken::new()).await });
    let original = client.owner_channel_binding().unwrap().clone();
    client._session.rekey_soon().await.unwrap();
    let (client, permit) = fixture.authenticate(client, StreamRole::Control).await;
    assert_eq!(client.owner_channel_binding().unwrap(), &original);
    let binding = Binding::new(
        fixture.approved.host.host.clone(),
        fixture.approved.account.clone(),
        "runtime-test".into(),
    )
    .unwrap();
    let mut client = ControlClient::on_authenticated_stream(client, binding, permit);
    let cancel = CancellationToken::new();
    let caps = client.handshake(&cancel).await.unwrap();
    assert!(caps.inventory);
    assert!(caps.attach && caps.detach);
    assert!(!caps.ensure && !caps.create);
    let inventory = client
        .request(OperationId::new(), Request::List { cursor: None }, &cancel)
        .await
        .unwrap();
    let Response::Inventory { page } = inventory.body else {
        panic!("inventory")
    };
    assert!(page.sessions.is_empty());
    let mut revoked = fixture.policy.clone();
    revoked.remote_access = false;
    revoked.host_epoch = Epoch::new(2).unwrap();
    *fixture.source.0.lock().await =
        OwnerVerifier::from_protected_permissions(revoked, &[fixture.key.public_key()]).unwrap();
    assert!(client
        .request(OperationId::new(), Request::List { cursor: None }, &cancel)
        .await
        .is_err());
    assert!(server.await.unwrap().is_err());
    drop(client);
    ssh.abort();
    let _ = ssh.await;
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires explicitly built owned native helper and fixture child"]
async fn native_authenticated_runtime_routes_two_ssh_terminals_without_cross_session_input() {
    use crate::host_control::tests::native_routes::NativeFixture;
    use festerm_host_protocol::IssuedOperation;
    use festerm_sessiond::host_control::ControlError;
    use std::sync::atomic::AtomicBool;

    let fixture = Fixture::new();
    let mut native = NativeFixture::new(true);
    native.policy = fixture.policy.clone();
    let sessions = native.control();
    let epoch = sessions
        .issuance_epoch(&fixture.policy.principal, |_| Ok::<_, ControlError>(()))
        .unwrap();
    let mut targets = Vec::new();
    for label in ["A", "B"] {
        let mut creation = native.creation();
        creation.recipe.args = vec![
            format!("emit:ready-{label}"),
            "read-line".into(),
            format!("echo:{label}"),
            "spin".into(),
        ];
        let created = sessions
            .ensure(
                &fixture.policy.principal,
                &IssuedOperation::new(epoch),
                &creation,
                false,
                &AtomicBool::new(false),
                |_| {
                    fixture
                        .source
                        .current_policy()
                        .map_err(|_| ControlError::Denied)
                },
            )
            .unwrap();
        targets.push(created.summary.target);
    }
    let runtime = Arc::new(HostRuntime::new(sessions, fixture.source.clone()));
    let (client, stream, control_ssh) = fixture.pair().await;
    let control_runtime = runtime.clone();
    let control_server = tokio::spawn(async move {
        control_runtime
            .serve_stream(stream, CancellationToken::new())
            .await
    });
    let (client, permit) = fixture.authenticate(client, StreamRole::Control).await;
    let binding = Binding::new(
        fixture.approved.host.host.clone(),
        fixture.approved.account.clone(),
        "owned-native-runtime".into(),
    )
    .unwrap();
    let mut control = ControlClient::on_authenticated_stream(client, binding.clone(), permit);
    let cancel = CancellationToken::new();
    assert!(control.handshake(&cancel).await.unwrap().inventory);
    let page =
        crate::host_client::inventory_authenticated(&mut control, OperationId::new(), &cancel)
            .await
            .unwrap();
    assert_eq!(page.sessions.len(), 2);
    assert!(page.sessions.iter().all(|session| !session.attached));

    let mut live = Vec::new();
    let mut frontends = Vec::new();
    let mut servers = Vec::new();
    let mut connections = Vec::new();
    for target in targets {
        let (client, stream, ssh) = fixture.pair().await;
        connections.push(ssh);
        let worker = runtime.clone();
        servers.push(tokio::spawn(async move {
            worker.serve_stream(stream, CancellationToken::new()).await
        }));
        let (client, permit) = fixture.authenticate(client, StreamRole::Terminal).await;
        let scope = crate::host_client::Scope {
            binding: binding.clone(),
            request: OperationId::new(),
            generation: 1,
        };
        let (events, receiver) = flume::bounded(2);
        let worker_scope = scope.clone();
        let worker_cancel = cancel.clone();
        let worker = tokio::spawn(async move {
            crate::host_client::attach_authenticated(
                client,
                permit,
                &worker_scope,
                target,
                &worker_cancel,
                &events,
                &(Arc::new(|| {}) as Arc<dyn Fn() + Send + Sync>),
            )
            .await
        });
        let completion = receiver.recv_async().await.unwrap();
        assert_eq!(completion.scope, scope);
        let crate::host_client::Outcome::Snapshot(adoption) = completion.outcome else {
            panic!("frontend snapshot must precede Attached")
        };
        let mut presented =
            festerm_core::Terminal::new(festerm_core::Dimensions::new(80, 24).unwrap()).unwrap();
        adoption.adopt_into(&scope, &mut presented).unwrap();
        let crate::host_client::Outcome::Attached(handle) = worker.await.unwrap().unwrap() else {
            panic!("actual host activation must confirm attachment")
        };
        frontends.push(presented);
        live.push(handle);
    }
    live[0].input(b"alpha\n").unwrap();
    live[1].input(b"beta\n").unwrap();
    for (index, expected, foreign) in [
        (0, b"A:alpha".as_slice(), b"beta".as_slice()),
        (1, b"B:beta".as_slice(), b"alpha".as_slice()),
    ] {
        let observed = tokio::time::timeout(Duration::from_secs(8), async {
            let mut observed = Vec::new();
            loop {
                let frame = live[index].next_event().await.unwrap();
                if frame.kind() == crate::protocol::OUTPUT {
                    frontends[index].ingest(frame.payload());
                    observed.extend_from_slice(frame.payload());
                    assert!(observed.len() <= festerm_host_protocol::MAX_BYTES_PER_CLIENT);
                    if observed
                        .windows(expected.len())
                        .any(|bytes| bytes == expected)
                    {
                        break observed;
                    }
                }
            }
        })
        .await
        .unwrap();
        assert!(!observed
            .windows(foreign.len())
            .any(|bytes| bytes == foreign));
        let text = frontends[index].text_snapshot();
        assert!(text.text().contains(std::str::from_utf8(expected).unwrap()));
        assert!(!text.text().contains(std::str::from_utf8(foreign).unwrap()));
    }
    let inventory_operation = OperationId::new();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            match control
                .request(
                    inventory_operation.clone(),
                    Request::List { cursor: None },
                    &cancel,
                )
                .await
                .unwrap()
                .body
            {
                Response::Inventory { page } => {
                    assert_eq!(page.sessions.len(), 2);
                    assert!(page.sessions.iter().all(|session| session.attached));
                    break;
                }
                Response::Refused {
                    reason: festerm_host_protocol::Refusal::Busy,
                } => tokio::time::sleep(Duration::from_millis(10)).await,
                response => panic!("independent inventory failed: {response:?}"),
            }
        }
    })
    .await
    .unwrap();
    for terminal in &mut live {
        let operation = terminal.request_detach().unwrap();
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let frame = terminal.next_event().await.unwrap();
                if frame.kind() == crate::host_terminal::HOST_REPLY {
                    let response: Envelope<Response> =
                        festerm_host_protocol::decode(frame.payload()).unwrap();
                    assert_eq!(response.operation, operation);
                    assert!(matches!(response.body, Response::Detached { .. }));
                    break;
                }
            }
        })
        .await
        .unwrap();
    }
    drop(live);
    for server in servers {
        assert!(server.await.unwrap().is_ok());
    }
    assert_eq!(
        festerm_sessiond::list_running_sessions_in(&native.root)
            .unwrap()
            .len(),
        2
    );
    drop(control);
    assert!(control_server.await.unwrap().is_err());
    connections.push(control_ssh);
    for ssh in connections {
        ssh.abort();
        let _ = ssh.await;
    }
    // NativeFixture ends only these exact owned sessions and waits for their
    // recorded processes before removing its protected reconciliation state.
    drop(native);
}

#[tokio::test]
async fn authenticated_runtime_terminal_role_uses_separate_bounded_framing() {
    let fixture = Fixture::new();
    let (client, stream, ssh) = fixture.pair().await;
    let runtime = fixture.runtime.clone();
    let server =
        tokio::spawn(async move { runtime.serve_stream(stream, CancellationToken::new()).await });
    let (mut client, permit) = fixture.authenticate(client, StreamRole::Terminal).await;
    let request = Envelope::new(
        fixture.approved.host.host.clone(),
        OperationId::new(),
        Request::Capabilities {},
    );
    crate::host_terminal::write_contract(
        &mut client,
        &permit,
        crate::host_terminal::HOST_REQUEST,
        &request,
    )
    .await
    .unwrap();
    let frame = crate::host_terminal::read_frame(&mut client, &permit, false)
        .await
        .unwrap();
    assert_eq!(frame.kind(), crate::host_terminal::HOST_REPLY);
    let reply: Envelope<Response> = festerm_host_protocol::decode(frame.payload()).unwrap();
    reply.validate_for(&request).unwrap();
    let Response::Capabilities { capabilities } = reply.body else {
        panic!("terminal capabilities")
    };
    assert!(capabilities.attach && capabilities.detach);
    assert!(!capabilities.inventory && !capabilities.ensure && !capabilities.create);
    drop(frame);
    drop(client);
    assert!(server.await.unwrap().is_err());
    ssh.abort();
    let _ = ssh.await;
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires explicitly built owned native helper and fixture child"]
async fn native_authenticated_runtime_creation_retains_issuer_and_exact_end_authority() {
    use festerm_host_protocol::{EnsureDisposition, Refusal};

    let mut fixture = NativeRuntimeFixture::new();
    let mut control = fixture.control().await;
    let creation = fixture.native.creation();
    let issued = control.issue_operation().unwrap();
    assert!(control
        .request(
            OperationId::new(),
            Request::Create {
                creation: creation.clone(),
            },
            &CancellationToken::new(),
        )
        .await
        .is_err());
    assert!(
        festerm_sessiond::list_running_sessions_in(&fixture.native.root)
            .unwrap()
            .is_empty()
    );
    let created = issued_request(
        &mut control,
        &issued,
        Request::Create {
            creation: creation.clone(),
        },
    )
    .await;
    let Response::Ensured { session, .. } = created else {
        panic!("remote create: {created:?}")
    };
    let target = session.target;
    let Response::Ensured { session, .. } = issued_request(
        &mut control,
        &issued,
        Request::Create {
            creation: creation.clone(),
        },
    )
    .await
    else {
        panic!("unchanged issued retry must resolve the committed generation")
    };
    assert_eq!(session.target, target);
    let resume = control.issue_operation().unwrap();
    assert!(matches!(
        issued_request(
            &mut control,
            &resume,
            Request::Ensure {
                creation: creation.clone(),
            },
        )
        .await,
        Response::Ensured {
            disposition: EnsureDisposition::Resumed,
            ..
        }
    ));
    assert_eq!(
        festerm_sessiond::list_running_sessions_in(&fixture.native.root)
            .unwrap()
            .len(),
        1
    );
    let mut stale = target.clone();
    stale.generation =
        festerm_host_protocol::GenerationId::new(target.generation.get() + 1).unwrap();
    let context = fixture.client_context();
    let stale = Arc::new(crate::host_client::EndIntent::new(&context, stale));
    assert!(matches!(
        client_end_worker(&mut fixture, stale.clone()).await,
        crate::host_client::Outcome::Failed(crate::host_client::Failure::Control(
            festerm_tunnel_client::host_control::Error::Refused(Refusal::Stale)
        ))
    ));
    assert!(stale.is_issued() && !stale.is_resolved());
    let end = Arc::new(crate::host_client::EndIntent::new(&context, target.clone()));
    let original_end = end.operation().clone();
    assert!(matches!(
        client_end_worker(&mut fixture, end.clone()).await,
        crate::host_client::Outcome::Failed(crate::host_client::Failure::Control(
            festerm_tunnel_client::host_control::Error::Refused(Refusal::OutcomeUncertain)
        ))
    ));
    assert!(
        end.is_issued() && !end.is_resolved(),
        "durable stop intent is not observed Ended"
    );
    tokio::time::timeout(Duration::from_secs(8), async {
        while !festerm_sessiond::list_running_sessions_in(&fixture.native.root)
            .unwrap()
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let response = control
                .request(
                    OperationId::new(),
                    Request::Lookup {
                        logical: target.logical.clone(),
                    },
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
            if matches!(
                response.body,
                Response::Lookup {
                    result: festerm_host_protocol::LookupResult::Absent { .. }
                }
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let recreate = control.issue_operation().unwrap();
    let response = issued_request(
        &mut control,
        &recreate,
        Request::Ensure {
            creation: creation.clone(),
        },
    )
    .await;
    let Response::Ensured { session, .. } = response else {
        panic!("known native absence must permit explicit unchanged ensure: {response:?}")
    };
    assert_eq!(session.target.logical, target.logical);
    assert_ne!(session.target.generation, target.generation);
    assert!(matches!(
        client_end_worker(&mut fixture, end.clone()).await,
        crate::host_client::Outcome::Failed(crate::host_client::Failure::Control(
            festerm_tunnel_client::host_control::Error::Refused(Refusal::Stale)
        ))
    ));
    assert_eq!(end.operation(), &original_end);
    assert!(!end.is_resolved());
    assert_eq!(
        festerm_sessiond::list_running_sessions_in(&fixture.native.root)
            .unwrap()
            .len(),
        1,
        "retrying original End must not stop the replacement generation"
    );
    drop(control);
    fixture.auth.policy.capabilities.creation_allowed = false;
    fixture.auth.policy.grant_epoch = fixture.auth.policy.grant_epoch.next().unwrap();
    *fixture.auth.source.0.lock().await = OwnerVerifier::from_protected_permissions(
        fixture.auth.policy.clone(),
        &[fixture.auth.key.public_key()],
    )
    .unwrap();
    let mut control = fixture.control().await;
    let resume = control.issue_operation().unwrap();
    assert!(matches!(
        issued_request(&mut control, &resume, Request::Ensure { creation }).await,
        Response::Ensured {
            disposition: EnsureDisposition::Resumed,
            ..
        }
    ));
    let virgin = fixture.native.creation();
    for request in [
        Request::Create {
            creation: virgin.clone(),
        },
        Request::Ensure { creation: virgin },
    ] {
        let denied = control.issue_operation().unwrap();
        assert!(matches!(
            issued_request(&mut control, &denied, request).await,
            Response::Refused {
                reason: Refusal::Denied
            }
        ));
    }
    assert_eq!(
        festerm_sessiond::list_running_sessions_in(&fixture.native.root)
            .unwrap()
            .len(),
        1,
        "creation-disabled resume must not authorize virgin execution"
    );
    drop(control);
    fixture.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires explicitly built owned native helper and fixture child"]
async fn native_authenticated_runtime_takeover_waits_for_adoption_and_fences_old_generation() {
    use festerm_host_protocol::{AttachmentId, GenerationId, Refusal};

    let mut fixture = NativeRuntimeFixture::new();
    let mut control = fixture.control().await;
    let mut creation = fixture.native.creation();
    creation.recipe.args = vec![
        "read-line".into(),
        "echo:old-writer".into(),
        "read-line".into(),
        "echo:new-writer".into(),
        "spin".into(),
    ];
    let attachment = creation.attachment.clone();
    let target = create_remote(&mut control, creation).await;
    let mut pending = fixture.pending(target.clone(), attachment, false).await;
    pending.take_snapshot().unwrap();
    let mut old = pending
        .adopt(&CancellationToken::new())
        .await
        .unwrap()
        .spawn()
        .unwrap();
    let (client, permit) = fixture.stream(StreamRole::Terminal).await;
    let mut stale = target.clone();
    stale.generation = GenerationId::new(target.generation.get() + 1).unwrap();
    assert!(matches!(
        crate::host_terminal::establish(
            client,
            permit,
            fixture.binding(),
            OperationId::new(),
            stale,
            AttachmentId::new(),
            false,
            &CancellationToken::new(),
        )
        .await,
        Err(Error::Refused(Refusal::Stale))
    ));
    let mut candidate = fixture.pending(target, AttachmentId::new(), true).await;
    candidate.take_snapshot().unwrap();
    old.input(b"before-adoption\n").unwrap();
    terminal_output_until(&mut old, b"old-writer:before-adoption").await;
    let mut current = candidate
        .adopt(&CancellationToken::new())
        .await
        .unwrap()
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while old.next_event().await.is_ok() {}
    })
    .await
    .unwrap();
    assert!(old.input(b"must-not-replay\n").is_err());
    current.input(b"after-adoption\n").unwrap();
    let output = terminal_output_until(&mut current, b"new-writer:after-adoption").await;
    assert!(!output
        .windows(b"must-not-replay".len())
        .any(|bytes| bytes == b"must-not-replay"));
    drop((old, current, control));
    fixture.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires owned native helper and real 60-second heartbeat/grace deadlines"]
async fn native_authenticated_runtime_temporary_heartbeat_and_transport_loss_are_daemon_owned() {
    let mut fixture = NativeRuntimeFixture::new();
    let mut control = fixture.control().await;
    let mut creation = fixture.native.creation();
    creation.lifetime = festerm_host_protocol::Lifetime::Temporary {
        reconnect_grace_seconds: 60,
    };
    let attachment = creation.attachment.clone();
    let target = create_remote(&mut control, creation).await;
    let mut pending = fixture.pending(target.clone(), attachment, false).await;
    assert!(fixture.lease(&target)["pending"].is_object());
    pending.take_snapshot().unwrap();
    let terminal = pending
        .adopt(&CancellationToken::new())
        .await
        .unwrap()
        .spawn()
        .unwrap();
    assert!(terminal.binding().lease.is_some());
    let initial = fixture.lease(&target);
    assert_eq!(initial["connected"], true);
    assert!(initial["pending"].is_null());
    let initial_deadline = initial["deadline_ms"].as_u64().unwrap();
    tokio::time::sleep(Duration::from_secs(65)).await;
    let renewed = fixture.lease(&target);
    assert_eq!(renewed["connected"], true);
    assert!(renewed["deadline_ms"].as_u64().unwrap() > initial_deadline + 40_000);
    assert_eq!(
        festerm_sessiond::list_running_sessions_in(&fixture.native.root)
            .unwrap()
            .len(),
        1
    );
    let held_policy = fixture.auth.source.0.clone().lock_owned().await;
    drop(terminal);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(fixture.lease(&target)["connected"], true);
    drop(held_policy);
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.lease(&target)["connected"] != false {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let lost = fixture.lease(&target);
    assert_eq!(lost["stopped"], false);
    assert_eq!(lost["temporary"], renewed["temporary"]);
    assert_eq!(lost["writer_epoch"], renewed["writer_epoch"]);
    assert_eq!(
        festerm_sessiond::list_running_sessions_in(&fixture.native.root)
            .unwrap()
            .len(),
        1,
        "transport loss must preserve the real reconnect grace"
    );
    tokio::time::timeout(Duration::from_secs(65), async {
        while !festerm_sessiond::list_running_sessions_in(&fixture.native.root)
            .unwrap()
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    drop(control);
    fixture.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires explicitly built owned native helper and fixture child"]
async fn native_authenticated_runtime_live_policy_fences_actions_without_stopping_shells() {
    use festerm_session::{Session, SessionEvent, TerminalSize};

    for revocation in ["key", "host", "session"] {
        let mut fixture = NativeRuntimeFixture::new();
        let mut control = fixture.control().await;
        let mut creation = fixture.native.creation();
        creation.recipe.args = vec![
            "read-line".into(),
            "echo:forbidden-input".into(),
            "spin".into(),
        ];
        let attachment = creation.attachment.clone();
        let target = create_remote(&mut control, creation).await;
        let mut pending = fixture.pending(target.clone(), attachment, false).await;
        pending.take_snapshot().unwrap();
        let mut terminal = pending
            .adopt(&CancellationToken::new())
            .await
            .unwrap()
            .spawn()
            .unwrap();
        match revocation {
            "session" => fixture
                .native
                .control()
                .revoke_local(&fixture.auth.policy.principal, &target.logical)
                .unwrap(),
            _ => {
                let mut policy = fixture.auth.policy.clone();
                policy.grant_epoch = policy.grant_epoch.next().unwrap();
                if revocation == "host" {
                    policy.remote_access = false;
                }
                let keys = if revocation == "key" {
                    vec![crate::owner_auth::OwnerSigningKey::from_seed(&[29; 32]).public_key()]
                } else {
                    vec![fixture.auth.key.public_key()]
                };
                *fixture.auth.source.0.lock().await =
                    OwnerVerifier::from_protected_permissions(policy, &keys).unwrap();
            }
        }
        let _ = terminal.input(b"must-not-execute\n");
        tokio::time::timeout(Duration::from_secs(3), async {
            while let Ok(frame) = terminal.next_event().await {
                assert_ne!(frame.kind(), crate::protocol::OUTPUT);
            }
        })
        .await
        .unwrap();
        assert_eq!(
            festerm_sessiond::list_running_sessions_in(&fixture.native.root)
                .unwrap()
                .len(),
            1,
            "{revocation} must not stop a persistent daemon"
        );
        let observed = fixture
            .native
            .control()
            .observe_local(&fixture.auth.policy.principal, &target)
            .unwrap();
        assert_eq!(observed.revoked, revocation == "session");
        // Persistent local unticketed attachment remains permitted. It is not
        // remote admission and cannot grant a new proof or temporary lease.
        let selected = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(selected) =
                    festerm_sessiond::list_unattached_sessions_in(&fixture.native.root)
                        .unwrap()
                        .into_iter()
                        .find(|session| session.created_at_unix_ms == target.generation.get())
                {
                    break selected;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let local = festerm_sessiond::PersistentSession::resume_discovered_in(
            &selected,
            &fixture.native.root,
            festerm_session::noop_session_event_notifier(),
        );
        assert!(local.is_ok(), "{revocation}: local persistent attachment");
        let local = local.unwrap();
        let snapshot = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(snapshot) = local.take_recovered_terminal_deferred() {
                    break snapshot;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!snapshot.text_snapshot().text().contains("must-not-execute"));
        local.acknowledge_recovery_adoption();
        let size = TerminalSize::new(81, 25).unwrap();
        local.try_resize(size).unwrap();
        local.try_send_input(b"local-approved\n").unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut observed = Vec::new();
            loop {
                match local.try_recv_event() {
                    Ok(SessionEvent::Output(bytes)) => {
                        observed.extend_from_slice(&bytes);
                        if observed.windows(b"forbidden-input:local-approved".len()).any(|bytes| bytes == b"forbidden-input:local-approved") {
                            assert!(!observed.windows(b"must-not-execute".len()).any(|bytes| bytes == b"must-not-execute"));
                            break;
                        }
                    }
                    Ok(SessionEvent::Lifecycle(state)) if state.is_terminal() => {
                        panic!("{revocation}: local writer activation failed: {state:?}");
                    }
                    _ => {}
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{revocation}: local activation timed out, lifecycle={:?}, lease={:?}, sessions={:?}", local.lifecycle(), fixture.lease(&target), festerm_sessiond::list_running_sessions_in(&fixture.native.root)));
        drop(local);
        fixture
            .native
            .control()
            .end_local(&fixture.auth.policy.principal, &target)
            .unwrap();
        drop((terminal, control));
        fixture.shutdown().await;
    }
}

#[tokio::test]
async fn authenticated_runtime_rejects_proof_replayed_on_another_actual_ssh_channel() {
    let fixture = Fixture::new();
    let (mut a, stream_a, ssh_a) = fixture.pair().await;
    let (mut b, stream_b, ssh_b) = fixture.pair().await;
    let stop = CancellationToken::new();
    let runtime_a = fixture.runtime.clone();
    let cancel_a = stop.clone();
    let server_a = tokio::spawn(async move { runtime_a.serve_stream(stream_a, cancel_a).await });
    let budgets = Budgets::default();
    let frame = read_frame(&mut a, &budgets).await.unwrap();
    let Authentication::Challenge { challenge } = frame.decode().unwrap() else {
        panic!("challenge")
    };
    drop(frame);
    // Keep the first exchange pending without holding its policy lease. This
    // tests proof replay, not legitimate refusal of concurrent policy reads.
    let runtime_b = fixture.runtime.clone();
    let server_b = tokio::spawn(async move {
        runtime_b
            .serve_stream(stream_b, CancellationToken::new())
            .await
    });
    let proof = a
        .prove_owner_fixture(
            fixture.vault.as_ref(),
            &fixture.key,
            &fixture.approved,
            &challenge,
        )
        .unwrap();
    let frame = read_frame(&mut b, &budgets).await.unwrap();
    assert!(matches!(
        frame.decode().unwrap(),
        Authentication::Challenge { .. }
    ));
    drop(frame);
    write_frame(&mut b, &budgets, &Authentication::Proof { proof })
        .await
        .unwrap();
    let frame = read_frame(&mut b, &budgets).await.unwrap();
    assert!(matches!(frame.decode().unwrap(), Authentication::Denied {}));
    drop(frame);
    assert_eq!(
        server_b.await.unwrap(),
        Err(Error::Contract(ContractError::Denied))
    );
    stop.cancel();
    assert_eq!(server_a.await.unwrap(), Err(Error::Cancelled));
    drop((a, b));
    ssh_a.abort();
    ssh_b.abort();
    let _ = tokio::join!(ssh_a, ssh_b);
}

#[tokio::test]
async fn authenticated_runtime_bounds_proof_frames_and_client_admission() {
    let fixture = Fixture::new();
    let (mut client, stream, ssh) = fixture.pair().await;
    let runtime = fixture.runtime.clone();
    let server =
        tokio::spawn(async move { runtime.serve_stream(stream, CancellationToken::new()).await });
    let frame = read_frame(&mut client, &Budgets::default()).await.unwrap();
    assert!(matches!(
        frame.decode().unwrap(),
        Authentication::Challenge { .. }
    ));
    drop(frame);
    client
        .write_u32(festerm_host_protocol::CONTROL_FRAME_BYTES as u32 + 1)
        .await
        .unwrap();
    client.flush().await.unwrap();
    assert_eq!(
        server.await.unwrap(),
        Err(Error::Contract(ContractError::Limit))
    );
    drop(client);
    ssh.abort();
    let _ = ssh.await;
    let held: Vec<_> = (0..MAX_CLIENTS)
        .map(|_| ConnectionPermit::reserve(&fixture.runtime.budgets).unwrap())
        .collect();
    let (client, stream, ssh) = fixture.pair().await;
    assert_eq!(
        fixture
            .runtime
            .serve_stream(stream, CancellationToken::new())
            .await,
        Err(Error::Contract(ContractError::Busy))
    );
    drop((held, client));
    ssh.abort();
    let _ = ssh.await;
}
