use super::*;
use crate::owner_auth::{OwnerPolicy, OwnerSigningKey, PinnedHost};
use festerm_host_protocol::{
    AuthorizedInventory, Budgets, Capabilities, GenerationId, HostId, InventoryEntry, Lifetime,
    LogicalSessionId, LogicalState, LookupResult, OperationId, PrincipalId, SessionSummary, Target,
    MAX_CLIENTS,
};
use festerm_tunnel_client::host_control::{Binding, ControlClient};
use std::sync::{Condvar, Mutex};
use tokio::io::duplex;

struct Policy(Arc<tokio::sync::Mutex<OwnerVerifier>>);
struct FixturePolicyGuard(tokio::sync::OwnedMutexGuard<OwnerVerifier>);
impl PolicyGuard for FixturePolicyGuard {
    fn verifier(&self) -> &OwnerVerifier {
        &self.0
    }
}
impl ProtectedPolicySource for Policy {
    type Guard = FixturePolicyGuard;
    fn current_policy(&self) -> Result<Self::Guard, ContractError> {
        self.0
            .clone()
            .try_lock_owned()
            .map(FixturePolicyGuard)
            .map_err(|_| ContractError::Busy)
    }
}

fn policy() -> OwnerPolicy {
    OwnerPolicy {
        host: PinnedHost {
            host: HostId::new(),
            host_key_fingerprint: format!("SHA256:{}", "a".repeat(43)),
        },
        principal: PrincipalId::new(),
        host_epoch: Epoch::new(1).unwrap(),
        grant_epoch: Epoch::new(1).unwrap(),
        remote_access: true,
        capabilities: Capabilities {
            inventory: true,
            ..Capabilities::default()
        },
    }
}

fn verified(policy: &OwnerPolicy) -> (VerifiedOwner, OwnerConnectionGuard, Arc<Policy>) {
    let key = OwnerSigningKey::from_seed(&[17; 32]);
    let verifier = OwnerVerifier::new(policy.clone(), &[key.public_key()]).unwrap();
    let challenge = verifier.challenge().unwrap();
    let proof = key.prove(challenge.challenge(), &policy.host).unwrap();
    let (owner, guard) = challenge.verify(&proof, &verifier).unwrap();
    (
        owner,
        guard,
        Arc::new(Policy(Arc::new(tokio::sync::Mutex::new(verifier)))),
    )
}

fn grant(owner: &PrincipalId, shared: bool) -> SessionGrant {
    SessionGrant {
        logical: LogicalSessionId::new(),
        owner: owner.clone(),
        remote_access: shared,
        state: LogicalState::Running(GenerationId::new(7).unwrap()),
        revision: Epoch::new(1).unwrap(),
    }
}

fn summary(grant: &SessionGrant) -> SessionSummary {
    let LogicalState::Running(generation) = grant.state else {
        panic!("fixture running")
    };
    SessionSummary {
        target: Target {
            logical: grant.logical.clone(),
            generation,
        },
        name: "fixture".into(),
        lifetime: Lifetime::Persistent {},
        attached: false,
    }
}

/// Protected fixture table, not a production registry or spawn backend.
struct Fixture {
    grants: Mutex<Vec<SessionGrant>>,
    captured: tokio::sync::Notify,
    pause: Mutex<bool>,
    release: Condvar,
}

impl Fixture {
    fn new(grants: Vec<SessionGrant>, pause: bool) -> Arc<Self> {
        Arc::new(Self {
            grants: Mutex::new(grants),
            captured: tokio::sync::Notify::new(),
            pause: Mutex::new(pause),
            release: Condvar::new(),
        })
    }

    fn release(&self) {
        *self.pause.lock().unwrap() = false;
        self.release.notify_all();
    }
}

fn refuse(error: ContractError) -> Refusal {
    match error {
        ContractError::Stale => Refusal::Stale,
        _ => Refusal::Denied,
    }
}

impl SessionBackend for Fixture {
    fn execute(
        &self,
        request: &Envelope<Request>,
        admission: &OperationAdmission,
    ) -> Result<Response, Refusal> {
        let epoch = Epoch::new(1).unwrap();
        match &request.body {
            Request::Capabilities {} => {
                admission.capture(None, epoch).map_err(refuse)?;
                Ok(Response::Capabilities {
                    capabilities: Capabilities {
                        inventory: true,
                        ..Capabilities::default()
                    },
                })
            }
            Request::List { cursor: None } => {
                let fence = admission.capture(None, epoch).map_err(refuse)?;
                let records = self.grants.lock().unwrap();
                let entries = records
                    .iter()
                    .filter(|grant| matches!(grant.state, LogicalState::Running(_)))
                    .map(|grant| InventoryEntry {
                        owner: grant.owner.clone(),
                        shared: grant.remote_access,
                        revoked: false,
                        session: summary(grant),
                    });
                let snapshot = AuthorizedInventory::capture(
                    request.host.clone(),
                    admission.principal().clone(),
                    fence.grant_epoch,
                    request.operation.clone(),
                    entries,
                )
                .map_err(refuse)?;
                let page = snapshot
                    .page(
                        &request.host,
                        admission.principal(),
                        fence.grant_epoch,
                        None,
                    )
                    .map_err(refuse)?;
                admission.revalidate(&fence, None, epoch).map_err(refuse)?;
                Ok(Response::Inventory { page })
            }
            Request::Lookup { logical } => {
                let captured = {
                    let records = self.grants.lock().unwrap();
                    let grant = records.iter().find(|grant| grant.logical == *logical);
                    admission.capture(grant, epoch).map_err(refuse)?
                };
                self.captured.notify_one();
                let pause = self.pause.lock().unwrap();
                let _pause = self
                    .release
                    .wait_timeout_while(pause, std::time::Duration::from_secs(3), |paused| *paused)
                    .unwrap();
                let records = self.grants.lock().unwrap();
                let grant = records.iter().find(|grant| grant.logical == *logical);
                admission
                    .revalidate(&captured, grant, epoch)
                    .map_err(refuse)?;
                let grant = grant.unwrap();
                Ok(Response::Lookup {
                    result: match grant.state {
                        LogicalState::Running(_) => LookupResult::Running {
                            session: summary(grant),
                        },
                        LogicalState::Absent => LookupResult::Absent {
                            logical: logical.clone(),
                            authorization_epoch: captured.grant_epoch,
                        },
                        _ => return Err(Refusal::Denied),
                    },
                })
            }
            _ => Err(Refusal::Unsupported),
        }
    }
}

async fn client(
    policy: &OwnerPolicy,
    fixture: Arc<Fixture>,
    source: Option<Arc<Policy>>,
) -> (
    ControlClient<tokio::io::DuplexStream>,
    tokio::task::JoinHandle<Result<(), Error>>,
    Arc<Policy>,
) {
    let (owner, guard, default_source) = verified(policy);
    let source = source.unwrap_or(default_source);
    let (stream, peer) = duplex(4096);
    let server = tokio::spawn(serve_control(
        peer,
        ConnectionPermit::reserve(&Budgets::default()).unwrap(),
        owner,
        guard,
        source.clone(),
        fixture,
        CancellationToken::new(),
    ));
    let binding = Binding::new(
        policy.host.host.clone(),
        "account".into(),
        "endpoint".into(),
    )
    .unwrap();
    let mut client = ControlClient::on_authenticated_stream(
        stream,
        binding,
        ConnectionPermit::reserve(&Budgets::default()).unwrap(),
    );
    client.handshake(&CancellationToken::new()).await.unwrap();
    (client, server, source)
}

#[tokio::test]
async fn real_proof_framed_inventory_filters_private_foreign_and_unknown_lookup() {
    let policy = policy();
    let owned = grant(&policy.principal, true);
    let private = grant(&policy.principal, false);
    let foreign = grant(&PrincipalId::new(), true);
    let fixture = Fixture::new(vec![owned.clone(), private.clone(), foreign.clone()], false);
    let (mut client, server, _) = client(&policy, fixture, None).await;
    let cancel = CancellationToken::new();
    let response = client
        .request(OperationId::new(), Request::List { cursor: None }, &cancel)
        .await
        .unwrap();
    let Response::Inventory { page } = response.body else {
        panic!("inventory")
    };
    assert_eq!(page.sessions, vec![summary(&owned)]);
    for logical in [private.logical, foreign.logical, LogicalSessionId::new()] {
        let response = client
            .request(OperationId::new(), Request::Lookup { logical }, &cancel)
            .await
            .unwrap();
        assert_eq!(
            response.body,
            Response::Refused {
                reason: Refusal::Denied
            }
        );
    }
    drop(client);
    assert!(server.await.unwrap().is_err());
}

#[tokio::test]
async fn revision_or_generation_change_while_pending_fences_publication() {
    for replacement in [false, true] {
        let policy = policy();
        let owned = grant(&policy.principal, true);
        let fixture = Fixture::new(vec![owned.clone()], true);
        let (mut client, server, _) = client(&policy, fixture.clone(), None).await;
        let request = tokio::spawn(async move {
            client
                .request(
                    OperationId::new(),
                    Request::Lookup {
                        logical: owned.logical,
                    },
                    &CancellationToken::new(),
                )
                .await
        });
        fixture.captured.notified().await;
        {
            let mut grants = fixture.grants.lock().unwrap();
            grants[0].revision = grants[0].revision.next().unwrap();
            if replacement {
                grants[0].state = LogicalState::Running(GenerationId::new(8).unwrap());
            }
        }
        fixture.release();
        assert_eq!(
            request.await.unwrap().unwrap().body,
            Response::Refused {
                reason: Refusal::Stale
            }
        );
        assert!(server.await.unwrap().is_err());
    }
}

#[tokio::test]
async fn protected_revocation_during_work_never_publishes_inventory_or_absence() {
    let policy = policy();
    let owned = grant(&policy.principal, true);
    let fixture = Fixture::new(vec![owned.clone()], true);
    let (mut client, server, source) = client(&policy, fixture.clone(), None).await;
    let request = tokio::spawn(async move {
        client
            .request(
                OperationId::new(),
                Request::Lookup {
                    logical: owned.logical,
                },
                &CancellationToken::new(),
            )
            .await
    });
    fixture.captured.notified().await;
    let mut revoked = policy;
    revoked.remote_access = false;
    revoked.grant_epoch = revoked.grant_epoch.next().unwrap();
    source
        .0
        .try_lock()
        .unwrap()
        .replace(
            revoked,
            &[OwnerSigningKey::from_seed(&[17; 32]).public_key()],
        )
        .unwrap();
    fixture.release();
    assert!(request.await.unwrap().is_err());
    assert!(server.await.unwrap().is_err());
}

#[test]
fn connection_admission_is_aggregate_and_released_by_raii() {
    let budgets = Budgets::default();
    let mut permits: Vec<_> = (0..MAX_CLIENTS)
        .map(|_| ConnectionPermit::reserve(&budgets).unwrap())
        .collect();
    assert!(ConnectionPermit::reserve(&budgets).is_err());
    permits.pop();
    assert!(ConnectionPermit::reserve(&budgets).is_ok());
}

#[cfg(all(feature = "relay", unix))]
mod native_routes {
    use super::*;
    use festerm_host_protocol::{AttachmentId, Creation, EnsureDisposition, Recipe};
    use festerm_sessiond::{
        host_control::{ControlError, SessionControl},
        private_storage,
    };
    use std::{
        path::PathBuf,
        time::{Duration, Instant},
    };

    struct NativeFixture {
        root: PathBuf,
        helper: PathBuf,
        policy: OwnerPolicy,
    }

    impl NativeFixture {
        fn new(creation_allowed: bool) -> Self {
            let helper = PathBuf::from(
                std::env::var_os("FESTERM_HOST_TEST_HELPER")
                    .expect("set FESTERM_HOST_TEST_HELPER to the owned built sessiond helper"),
            );
            assert!(helper.is_absolute() && helper.is_file());
            assert!(helper.with_file_name("festerm-pty-test-child").is_file());
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join(format!("hr-{}", &OperationId::new().as_str()[..8]));
            private_storage::private_directory(&root, true).unwrap();
            let mut policy = policy();
            policy.capabilities = Capabilities {
                inventory: true,
                ensure: true,
                create: true,
                attach: true,
                explicit_end: true,
                creation_allowed,
                ..Capabilities::default()
            };
            Self {
                root,
                helper,
                policy,
            }
        }

        fn control(&self) -> SessionControl {
            SessionControl::with_helper(&self.root, &self.helper).unwrap()
        }

        fn creation(&self) -> Creation {
            Creation {
                logical: LogicalSessionId::new(),
                attachment: AttachmentId::new(),
                name: Some("owned routing fixture".into()),
                recipe: Recipe {
                    shell: self
                        .helper
                        .with_file_name("festerm-pty-test-child")
                        .to_str()
                        .map(str::to_owned),
                    args: vec!["spin".into()],
                    cwd: self.root.to_str().map(str::to_owned),
                },
                lifetime: Lifetime::Persistent {},
            }
        }

        fn backend(&self) -> (Arc<native::NativeControl<Policy>>, Arc<Policy>) {
            let (_, _, source) = verified(&self.policy);
            (
                Arc::new(native::NativeControl::new(self.control(), source.clone())),
                source,
            )
        }

        async fn connect(
            &self,
            backend: Arc<dyn SessionBackend>,
            source: Arc<Policy>,
            budgets: &Budgets,
        ) -> (
            ControlClient<tokio::io::DuplexStream>,
            tokio::task::JoinHandle<Result<(), Error>>,
        ) {
            let (owner, guard, _) = verified(&self.policy);
            let (stream, peer) = duplex(4096);
            let server = tokio::spawn(serve_control(
                peer,
                ConnectionPermit::reserve(budgets).unwrap(),
                owner,
                guard,
                source,
                backend,
                CancellationToken::new(),
            ));
            let binding = Binding::new(
                self.policy.host.host.clone(),
                "owned-account".into(),
                "owned-endpoint".into(),
            )
            .unwrap();
            let mut client = ControlClient::on_authenticated_stream(
                stream,
                binding,
                ConnectionPermit::reserve(&Budgets::default()).unwrap(),
            );
            let caps = client.handshake(&CancellationToken::new()).await.unwrap();
            assert!(caps.inventory);
            assert!(!caps.attach && !caps.create && !caps.ensure);
            (client, server)
        }
    }

    impl Drop for NativeFixture {
        fn drop(&mut self) {
            if let Ok(sessions) = self
                .control()
                .inventory(&self.policy.principal, |_| Ok::<_, ControlError>(()))
            {
                for session in sessions {
                    let _ =
                        self.control()
                            .end(&self.policy.principal, &session.summary.target, |_| {
                                Ok::<_, ControlError>(())
                            });
                }
            }
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                match festerm_sessiond::list_running_sessions_in(&self.root) {
                    Ok(sessions) if sessions.is_empty() => {
                        std::fs::remove_dir_all(&self.root).unwrap();
                        break;
                    }
                    _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
                    _ => panic!(
                        "owned fixture did not stop; retained {}",
                        self.root.display()
                    ),
                }
            }
        }
    }

    #[tokio::test]
    #[ignore = "requires explicitly built owned native helper and fixture child"]
    async fn native_framed_ensure_create_inventory_and_exact_end_use_real_daemons() {
        let fixture = NativeFixture::new(true);
        let (backend, source) = fixture.backend();
        let (mut client, server) = fixture.connect(backend, source, &Budgets::default()).await;
        let cancel = CancellationToken::new();
        let mut targets = Vec::new();
        for create_only in [false, true] {
            let creation = fixture.creation();
            let body = if create_only {
                Request::Create {
                    creation: creation.clone(),
                }
            } else {
                Request::Ensure {
                    creation: creation.clone(),
                }
            };
            let operation = OperationId::new();
            // Deliberate low-level core test: creation is not advertised until
            // terminal routing and channel proof integration are complete.
            let first = client
                .request(operation.clone(), body.clone(), &cancel)
                .await
                .unwrap();
            let Response::Ensured {
                session,
                disposition,
                ..
            } = &first.body
            else {
                panic!("{first:?}")
            };
            assert_eq!(*disposition, EnsureDisposition::Created);
            assert!(!session.attached);
            targets.push(session.target.clone());
            let retried = client.request(operation, body, &cancel).await.unwrap();
            assert_eq!(retried, first);
            let found = client
                .request(
                    OperationId::new(),
                    Request::Lookup {
                        logical: creation.logical,
                    },
                    &cancel,
                )
                .await
                .unwrap();
            assert!(matches!(
                found.body,
                Response::Lookup {
                    result: LookupResult::Running { .. }
                }
            ));
        }
        let inventory = client
            .request(OperationId::new(), Request::List { cursor: None }, &cancel)
            .await
            .unwrap();
        let Response::Inventory { page } = inventory.body else {
            panic!("inventory")
        };
        assert_eq!(page.sessions.len(), 2);
        assert!(page.sessions.iter().all(|session| !session.attached));
        assert_eq!(
            festerm_sessiond::list_running_sessions_in(&fixture.root)
                .unwrap()
                .len(),
            2
        );
        let stale = Target {
            logical: targets[0].logical.clone(),
            generation: GenerationId::new(1).unwrap(),
        };
        assert_eq!(
            client
                .request(
                    OperationId::new(),
                    Request::ExplicitEnd { target: stale },
                    &cancel
                )
                .await
                .unwrap()
                .body,
            Response::Refused {
                reason: Refusal::Stale
            }
        );
        for target in targets {
            assert_eq!(
                client
                    .request(
                        OperationId::new(),
                        Request::ExplicitEnd {
                            target: target.clone()
                        },
                        &cancel
                    )
                    .await
                    .unwrap()
                    .body,
                Response::Refused {
                    reason: Refusal::OutcomeUncertain
                }
            );
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                let result = client
                    .request(
                        OperationId::new(),
                        Request::Lookup {
                            logical: target.logical.clone(),
                        },
                        &cancel,
                    )
                    .await
                    .unwrap();
                if matches!(
                    result.body,
                    Response::Lookup {
                        result: LookupResult::Absent { .. }
                    }
                ) {
                    break;
                }
                assert!(Instant::now() < deadline, "{result:?}");
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
        drop(client);
        assert!(server.await.unwrap().is_err());
    }

    #[tokio::test]
    #[ignore = "requires explicitly built owned native helper and fixture child"]
    async fn native_creation_denial_does_not_register_virgin_identity_or_claim_absence() {
        let fixture = NativeFixture::new(false);
        let (backend, source) = fixture.backend();
        let (mut client, server) = fixture.connect(backend, source, &Budgets::default()).await;
        let creation = fixture.creation();
        let cancel = CancellationToken::new();
        assert_eq!(
            client
                .request(
                    OperationId::new(),
                    Request::Ensure {
                        creation: creation.clone()
                    },
                    &cancel
                )
                .await
                .unwrap()
                .body,
            Response::Refused {
                reason: Refusal::Denied
            }
        );
        assert_eq!(
            client
                .request(
                    OperationId::new(),
                    Request::Lookup {
                        logical: creation.logical
                    },
                    &cancel
                )
                .await
                .unwrap()
                .body,
            Response::Refused {
                reason: Refusal::Denied
            }
        );
        assert!(festerm_sessiond::list_running_sessions_in(&fixture.root)
            .unwrap()
            .is_empty());
        drop(client);
        assert!(server.await.unwrap().is_err());
    }

    struct LostReply {
        native: Arc<native::NativeControl<Policy>>,
        first: AtomicBool,
        committed: tokio::sync::Notify,
        release: Condvar,
        paused: Mutex<bool>,
    }
    impl SessionBackend for LostReply {
        fn execute(
            &self,
            request: &Envelope<Request>,
            admission: &OperationAdmission,
        ) -> Result<Response, Refusal> {
            let result = self.native.execute(request, admission);
            if matches!(result, Ok(Response::Ensured { .. }))
                && matches!(request.body, Request::Ensure { .. })
                && self.first.swap(false, Ordering::AcqRel)
            {
                self.committed.notify_one();
                let pause = self.paused.lock().unwrap();
                let _pause = self
                    .release
                    .wait_timeout_while(pause, Duration::from_secs(3), |paused| *paused)
                    .unwrap();
            }
            result
        }
    }

    #[tokio::test]
    #[ignore = "requires explicitly built owned native helper and fixture child"]
    async fn native_lost_reply_disconnect_retry_and_independent_control_never_duplicate_spawn() {
        let fixture = NativeFixture::new(true);
        let (native, source) = fixture.backend();
        let backend = Arc::new(LostReply {
            native,
            first: AtomicBool::new(true),
            committed: tokio::sync::Notify::new(),
            release: Condvar::new(),
            paused: Mutex::new(true),
        });
        let budgets = Budgets::default();
        let (mut first, first_server) = fixture
            .connect(backend.clone(), source.clone(), &budgets)
            .await;
        let creation = fixture.creation();
        let operation = OperationId::new();
        let cancel = CancellationToken::new();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let request = first.request(
                operation.clone(),
                Request::Ensure {
                    creation: creation.clone(),
                },
                &cancel,
            );
            tokio::pin!(request);
            tokio::select! {
                result = &mut request => {
                    let response = result.unwrap();
                    assert!(matches!(response.body, Response::Refused {
                        reason: Refusal::Busy | Refusal::Unknown | Refusal::OutcomeUncertain
                    }), "reply unexpectedly published: {response:?}");
                    assert!(Instant::now() < deadline, "native create never reconciled: {response:?}");
                    tokio::time::sleep(Duration::from_millis(25)).await;
                },
                _ = backend.committed.notified() => break,
            }
        }
        drop(first);
        assert!(first_server.await.unwrap().is_err());
        let remaining_create = budgets.reserve(Resource::PendingCreate, 1).unwrap();
        assert!(budgets.reserve(Resource::PendingCreate, 1).is_err());
        drop(remaining_create);
        let (mut second, second_server) = fixture.connect(backend.clone(), source, &budgets).await;
        let result = second
            .request(
                operation,
                Request::Ensure {
                    creation: creation.clone(),
                },
                &cancel,
            )
            .await
            .unwrap();
        let Response::Ensured { session, .. } = result.body else {
            panic!("{result:?}")
        };
        assert_eq!(session.target.logical, creation.logical);
        let inventory = second
            .request(OperationId::new(), Request::List { cursor: None }, &cancel)
            .await
            .unwrap();
        let Response::Inventory { page } = inventory.body else {
            panic!("inventory")
        };
        assert_eq!(page.sessions.len(), 1);
        assert_eq!(page.sessions[0].target, session.target);
        assert_eq!(
            festerm_sessiond::list_running_sessions_in(&fixture.root)
                .unwrap()
                .len(),
            1
        );
        *backend.paused.lock().unwrap() = false;
        backend.release.notify_all();
        drop(second);
        assert!(second_server.await.unwrap().is_err());
    }
}
