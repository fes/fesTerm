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

pub(crate) struct Policy(pub(crate) Arc<tokio::sync::Mutex<OwnerVerifier>>);
pub(crate) struct FixturePolicyGuard(tokio::sync::OwnedMutexGuard<OwnerVerifier>);
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

pub(crate) fn verified(policy: &OwnerPolicy) -> (VerifiedOwner, OwnerConnectionGuard, Arc<Policy>) {
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
    unknown: Mutex<Vec<(SessionGrant, Target)>>,
    captured: tokio::sync::Notify,
    pause: Mutex<bool>,
    release: Condvar,
}

impl Fixture {
    fn new(grants: Vec<SessionGrant>, pause: bool) -> Arc<Self> {
        Arc::new(Self {
            grants: Mutex::new(grants),
            unknown: Mutex::new(Vec::new()),
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
                let unknown = self.unknown.lock().unwrap();
                let uncertain = unknown.iter().map(|(grant, target)| {
                    festerm_host_protocol::UnknownInventoryEntry {
                        owner: grant.owner.clone(),
                        shared: grant.remote_access,
                        revoked: matches!(grant.state, LogicalState::Revoked),
                        target: target.clone(),
                    }
                });
                let snapshot = AuthorizedInventory::capture_with_unknowns(
                    request.host.clone(),
                    admission.principal().clone(),
                    fence.grant_epoch,
                    request.operation.clone(),
                    entries,
                    uncertain,
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
async fn framed_partial_inventory_preserves_unknowns_without_private_or_foreign_targets() {
    let policy = policy();
    let healthy = grant(&policy.principal, true);
    let unknown = grant(&policy.principal, true);
    let expected = summary(&unknown).target;
    let fixture = Fixture::new(vec![healthy.clone()], false);
    for mut entry in [
        unknown,
        grant(&policy.principal, false),
        grant(&PrincipalId::new(), true),
        grant(&policy.principal, true),
    ] {
        let target = summary(&entry).target;
        if fixture.unknown.lock().unwrap().len() == 3 {
            entry.state = LogicalState::Revoked;
        }
        fixture.unknown.lock().unwrap().push((entry, target));
    }
    let (mut client, server, _) = client(&policy, fixture, None).await;
    let response = client
        .request(
            OperationId::new(),
            Request::List { cursor: None },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let Response::Inventory { page } = response.body else {
        panic!("inventory")
    };
    assert_eq!(page.sessions, vec![summary(&healthy)]);
    assert_eq!(page.unknown, vec![expected]);
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

async fn raw_connection(
    policy: &OwnerPolicy,
    backend: Arc<dyn SessionBackend>,
    source: Arc<Policy>,
    budgets: &Budgets,
) -> (
    tokio::io::DuplexStream,
    tokio::task::JoinHandle<Result<(), Error>>,
) {
    let (owner, guard, _) = verified(policy);
    let (mut stream, peer) = duplex(4096);
    let server = tokio::spawn(serve_control(
        peer,
        ConnectionPermit::reserve(budgets).unwrap(),
        owner,
        guard,
        source,
        backend,
        CancellationToken::new(),
    ));
    let response = raw_request(&mut stream, policy, Request::Capabilities {}).await;
    assert!(matches!(response.body, Response::Capabilities { .. }));
    (stream, server)
}

async fn raw_request(
    stream: &mut tokio::io::DuplexStream,
    policy: &OwnerPolicy,
    body: Request,
) -> Envelope<Response> {
    use festerm_tunnel_client::host_control::{read_frame, write_frame};
    let request = if matches!(body, Request::Ensure { .. } | Request::Create { .. }) {
        Envelope::issued(
            policy.host.host.clone(),
            &festerm_host_protocol::IssuedOperation::new(
                policy.capabilities.issuance_epoch.unwrap(),
            ),
            body,
        )
    } else {
        Envelope::new(policy.host.host.clone(), OperationId::new(), body)
    };
    let budgets = Budgets::default();
    write_frame(stream, &budgets, &request).await.unwrap();
    let response: Envelope<Response> = read_frame(stream, &budgets)
        .await
        .unwrap()
        .decode()
        .unwrap();
    response.validate_for(&request).unwrap();
    response
}

struct Counted {
    capabilities: Capabilities,
    effects: std::sync::atomic::AtomicUsize,
}
impl SessionBackend for Counted {
    fn execute(
        &self,
        request: &Envelope<Request>,
        admission: &OperationAdmission,
    ) -> Result<Response, Refusal> {
        if matches!(request.body, Request::Capabilities {}) {
            admission
                .capture(None, Epoch::new(1).unwrap())
                .map_err(refuse)?;
            return Ok(Response::Capabilities {
                capabilities: self.capabilities,
            });
        }
        self.effects.fetch_add(1, Ordering::AcqRel);
        Err(Refusal::Unknown)
    }
}

fn all_capabilities() -> Capabilities {
    Capabilities {
        inventory: true,
        ensure: true,
        create: true,
        attach: true,
        detach: true,
        explicit_end: true,
        temporary: true,
        creation_allowed: true,
        profiles: false,
        ensure_profile: false,
        issuance_epoch: Some(Epoch::new(1).unwrap()),
    }
}

fn creation() -> festerm_host_protocol::Creation {
    festerm_host_protocol::Creation {
        logical: LogicalSessionId::new(),
        attachment: festerm_host_protocol::AttachmentId::new(),
        name: None,
        recipe: festerm_host_protocol::Recipe::default(),
        lifetime: Lifetime::Persistent {},
    }
}

#[tokio::test]
async fn server_refuses_every_unadvertised_operation_before_backend_execution() {
    let mut policy = policy();
    policy.capabilities = all_capabilities();
    let (_, _, source) = verified(&policy);
    let backend = Arc::new(Counted {
        capabilities: Capabilities::default(),
        effects: std::sync::atomic::AtomicUsize::new(0),
    });
    let (mut stream, server) =
        raw_connection(&policy, backend.clone(), source, &Budgets::default()).await;
    let target = Target {
        logical: LogicalSessionId::new(),
        generation: GenerationId::new(1).unwrap(),
    };
    for request in [
        Request::List { cursor: None },
        Request::Lookup {
            logical: target.logical.clone(),
        },
        Request::Ensure {
            creation: creation(),
        },
        Request::Create {
            creation: creation(),
        },
        Request::Attach {
            target: target.clone(),
            attachment: festerm_host_protocol::AttachmentId::new(),
            takeover: false,
        },
        Request::Detach {
            target: target.clone(),
            attachment: festerm_host_protocol::AttachmentId::new(),
            lease: None,
            lease_epoch: None,
            reason: festerm_host_protocol::DetachKind::Intentional,
        },
        Request::ExplicitEnd { target },
    ] {
        assert_eq!(
            raw_request(&mut stream, &policy, request).await.body,
            Response::Refused {
                reason: Refusal::Unsupported
            }
        );
    }
    assert_eq!(backend.effects.load(Ordering::Acquire), 0);
    drop(stream);
    assert!(server.await.unwrap().is_err());
}

#[tokio::test]
async fn saturation_returns_correlated_busy_and_preserves_connection() {
    for resource in [
        Resource::Control,
        Resource::ControlBytes,
        Resource::PendingCreate,
    ] {
        let mut policy = policy();
        policy.capabilities = all_capabilities();
        let (_, _, source) = verified(&policy);
        let backend = Arc::new(Counted {
            capabilities: all_capabilities(),
            effects: std::sync::atomic::AtomicUsize::new(0),
        });
        let budgets = Budgets::default();
        let (mut stream, server) = raw_connection(&policy, backend.clone(), source, &budgets).await;
        let occupied = match resource {
            Resource::ControlBytes => vec![budgets
                .reserve(
                    resource,
                    festerm_host_protocol::MAX_CONTROL_BYTES - REFUSAL_BYTES - 1024,
                )
                .unwrap()],
            _ => (0..if resource == Resource::Control {
                festerm_host_protocol::MAX_CONTROL_OPERATIONS
            } else {
                festerm_host_protocol::MAX_PENDING_CREATES
            })
                .map(|_| budgets.reserve(resource, 1).unwrap())
                .collect(),
        };
        assert_eq!(
            raw_request(
                &mut stream,
                &policy,
                Request::Ensure {
                    creation: creation()
                }
            )
            .await
            .body,
            Response::Refused {
                reason: Refusal::Busy
            }
        );
        assert_eq!(backend.effects.load(Ordering::Acquire), 0);
        drop(occupied);
        assert_eq!(
            raw_request(
                &mut stream,
                &policy,
                Request::Ensure {
                    creation: creation()
                }
            )
            .await
            .body,
            Response::Refused {
                reason: Refusal::Unknown
            }
        );
        assert_eq!(backend.effects.load(Ordering::Acquire), 1);
        drop(stream);
        assert!(server.await.unwrap().is_err());
    }
}

#[tokio::test]
async fn two_connections_policy_contention_returns_busy_not_denial_or_eof() {
    let policy = policy();
    let (_, _, source) = verified(&policy);
    let backend = Fixture::new(vec![], false);
    let budgets = Budgets::default();
    let (mut a, a_server) =
        raw_connection(&policy, backend.clone(), source.clone(), &budgets).await;
    let (mut b, b_server) = raw_connection(&policy, backend, source.clone(), &budgets).await;
    let held = source.0.clone().try_lock_owned().unwrap();
    for stream in [&mut a, &mut b] {
        assert_eq!(
            raw_request(stream, &policy, Request::List { cursor: None })
                .await
                .body,
            Response::Refused {
                reason: Refusal::Busy
            }
        );
    }
    drop(held);
    for stream in [&mut a, &mut b] {
        assert!(matches!(
            raw_request(stream, &policy, Request::List { cursor: None })
                .await
                .body,
            Response::Inventory { .. }
        ));
    }
    drop((a, b));
    assert!(a_server.await.unwrap().is_err());
    assert!(b_server.await.unwrap().is_err());
}

#[tokio::test]
async fn idle_time_does_not_consume_fresh_exchange_deadline() {
    let policy = policy();
    let owned = grant(&policy.principal, true);
    let fixture = Fixture::new(vec![owned.clone()], true);
    let (mut client, server, _) = client(&policy, fixture.clone(), None).await;
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(20)).await;
    tokio::time::resume();
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
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        fixture.captured.notified(),
    )
    .await
    .unwrap();
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(12)).await;
    tokio::task::yield_now().await;
    assert!(!request.is_finished());
    tokio::time::resume();
    fixture.release();
    assert!(matches!(
        request.await.unwrap().unwrap().body,
        Response::Lookup {
            result: LookupResult::Running { .. }
        }
    ));
    assert!(server.await.unwrap().is_err());
}

struct PostGate {
    policy: Arc<Policy>,
    armed: AtomicBool,
    reached: tokio::sync::Notify,
    paused: Mutex<bool>,
    release: Condvar,
}
impl AuthorizationSource for PostGate {
    fn with_current(
        &self,
        check: &mut dyn FnMut(&OwnerVerifier) -> Result<(), ContractError>,
    ) -> Result<(), ContractError> {
        self.policy.with_current(check)?;
        if self.armed.swap(false, Ordering::AcqRel) {
            self.reached.notify_one();
            let paused = self.paused.lock().unwrap();
            let _paused = self
                .release
                .wait_timeout_while(paused, std::time::Duration::from_secs(3), |paused| *paused)
                .unwrap();
        }
        Ok(())
    }
}
struct ChargedReply {
    gate: Arc<PostGate>,
    budgets: Budgets,
    held: Mutex<Option<festerm_host_protocol::BudgetPermit>>,
}
impl SessionBackend for ChargedReply {
    fn execute(
        &self,
        request: &Envelope<Request>,
        admission: &OperationAdmission,
    ) -> Result<Response, Refusal> {
        admission
            .capture(None, Epoch::new(1).unwrap())
            .map_err(refuse)?;
        if matches!(request.body, Request::Capabilities {}) {
            return Ok(Response::Capabilities {
                capabilities: Capabilities {
                    inventory: true,
                    ..Capabilities::default()
                },
            });
        }
        *self.held.lock().unwrap() = Some(
            self.budgets
                .reserve(
                    Resource::ControlBytes,
                    festerm_host_protocol::MAX_CONTROL_BYTES - REFUSAL_BYTES - CONTROL_FRAME_BYTES,
                )
                .unwrap(),
        );
        self.gate.armed.store(true, Ordering::Release);
        Err(Refusal::OutcomeUncertain)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reply_charge_survives_worker_completion_and_postauthorization() {
    let policy = policy();
    let (owner, guard, source) = verified(&policy);
    let gate = Arc::new(PostGate {
        policy: source,
        armed: AtomicBool::new(false),
        reached: tokio::sync::Notify::new(),
        paused: Mutex::new(true),
        release: Condvar::new(),
    });
    let budgets = Budgets::default();
    let backend = Arc::new(ChargedReply {
        gate: gate.clone(),
        budgets: budgets.clone(),
        held: Mutex::new(None),
    });
    let (mut stream, peer) = duplex(4096);
    let server = tokio::spawn(serve_control(
        peer,
        ConnectionPermit::reserve(&budgets).unwrap(),
        owner,
        guard,
        gate.clone(),
        backend.clone(),
        CancellationToken::new(),
    ));
    raw_request(&mut stream, &policy, Request::Capabilities {}).await;
    let request = tokio::spawn(async move {
        raw_request(&mut stream, &policy, Request::List { cursor: None }).await
    });
    gate.reached.notified().await;
    assert!(matches!(
        budgets.reserve(Resource::ControlBytes, CONTROL_FRAME_BYTES),
        Err(ContractError::Busy)
    ));
    *gate.paused.lock().unwrap() = false;
    gate.release.notify_all();
    assert_eq!(
        request.await.unwrap().body,
        Response::Refused {
            reason: Refusal::OutcomeUncertain
        }
    );
    backend.held.lock().unwrap().take();
    assert!(server.await.unwrap().is_err());
}

#[cfg(all(feature = "relay", unix))]
pub(crate) mod native_routes {
    use super::*;
    use festerm_host_protocol::{AttachmentId, Creation, EnsureDisposition, Recipe};
    use festerm_sessiond::{
        host_control::{ControlError, SessionControl},
        private_storage,
    };
    use festerm_tunnel_client::host_control::IssuedControlOperation;
    use std::{
        path::PathBuf,
        time::{Duration, Instant},
    };

    pub(crate) struct NativeFixture {
        pub(crate) root: PathBuf,
        pub(crate) helper: PathBuf,
        pub(crate) policy: OwnerPolicy,
        directory: Option<private_storage::PrivateTestDirectory>,
    }

    impl NativeFixture {
        pub(crate) fn new(creation_allowed: bool) -> Self {
            let helper = PathBuf::from(
                std::env::var_os("FESTERM_HOST_TEST_HELPER")
                    .expect("set FESTERM_HOST_TEST_HELPER to the owned built sessiond helper"),
            );
            assert!(helper.is_absolute() && helper.is_file());
            assert!(helper.with_file_name("festerm-pty-test-child").is_file());
            let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .to_path_buf();
            let directory = private_storage::private_test_directory_in(&parent);
            let root = directory.path().to_path_buf();
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
            policy.capabilities.issuance_epoch = Some(
                SessionControl::with_helper(&root, &helper)
                    .unwrap()
                    .issuance_epoch(&policy.principal, |_| Ok::<_, ControlError>(()))
                    .unwrap(),
            );
            Self {
                root,
                helper,
                policy,
                directory: Some(directory),
            }
        }

        pub(crate) fn control(&self) -> SessionControl {
            SessionControl::with_helper(&self.root, &self.helper).unwrap()
        }

        pub(crate) fn checkpoint(&self, name: &str, limit: usize) -> serde_json::Value {
            let directory = private_storage::private_directory(&self.root, false).unwrap();
            let bytes =
                private_storage::read_private(&directory, std::path::Path::new(name), limit)
                    .unwrap();
            serde_json::from_slice(&bytes).unwrap()
        }

        fn daemons_stopped(&self) -> bool {
            let path = self.root.join("lifecycle");
            let directory = match private_storage::private_directory(&path, false) {
                Ok(directory) => directory,
                Err(error) => return error.kind() == std::io::ErrorKind::NotFound,
            };
            let entries = match std::fs::read_dir(path) {
                Ok(entries) => entries,
                Err(_) => return false,
            };
            for (index, entry) in entries.enumerate() {
                if index >= festerm_host_protocol::MAX_SESSIONS * 4 {
                    return false;
                }
                let Ok(entry) = entry else { return false };
                if entry
                    .path()
                    .extension()
                    .is_none_or(|extension| extension != "json")
                {
                    continue;
                }
                let Ok(bytes) = private_storage::read_private(
                    &directory,
                    std::path::Path::new(&entry.file_name()),
                    32 * 1024,
                ) else {
                    return false;
                };
                let Ok(history) =
                    serde_json::from_slice::<Vec<festerm_sessiond::lifecycle::Entry>>(&bytes)
                else {
                    return false;
                };
                if history.len() > 64
                    || history
                        .iter()
                        .any(|entry| festerm_sessiond::process_alive(entry.pid))
                {
                    return false;
                }
            }
            true
        }

        pub(crate) fn creation(&self) -> Creation {
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

        pub(crate) fn backend(&self) -> (Arc<native::NativeControl<Policy>>, Arc<Policy>) {
            let (_, _, source) = verified(&self.policy);
            (
                Arc::new(native::NativeControl::with_test_capabilities(
                    self.control(),
                    source.clone(),
                )),
                source,
            )
        }

        pub(crate) async fn connect(
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
            assert!(caps.attach && caps.create && caps.ensure);
            (client, server)
        }
    }

    impl Drop for NativeFixture {
        fn drop(&mut self) {
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                if let Ok(sessions) = self
                    .control()
                    .inventory(&self.policy.principal, |_| Ok::<_, ControlError>(()))
                {
                    for session in sessions.sessions {
                        let _ = self.control().end(
                            &self.policy.principal,
                            &session.summary.target,
                            |_| Ok::<_, ControlError>(()),
                        );
                    }
                }
                match festerm_sessiond::list_running_sessions_in(&self.root) {
                    Ok(sessions) if sessions.is_empty() && self.daemons_stopped() => {
                        drop(self.directory.take());
                        break;
                    }
                    _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
                    _ => {
                        // Keep protected state if an owned daemon is still
                        // alive; RAII must not erase its reconciliation root.
                        std::mem::forget(self.directory.take());
                        if std::thread::panicking() {
                            eprintln!(
                                "owned fixture did not stop; retained {}",
                                self.root.display()
                            );
                            return;
                        }
                        panic!(
                            "owned fixture did not stop; retained {}",
                            self.root.display()
                        );
                    }
                }
            }
        }
    }

    async fn issued_request_after_busy(
        client: &mut ControlClient<tokio::io::DuplexStream>,
        operation: &IssuedControlOperation,
        body: Request,
        cancel: &CancellationToken,
    ) -> festerm_host_protocol::Envelope<Response> {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let response = client
                .request_issued(operation, body.clone(), cancel)
                .await
                .unwrap();
            if !matches!(
                response.body,
                Response::Refused {
                    reason: Refusal::Busy
                }
            ) {
                return response;
            }
            assert!(
                Instant::now() < deadline,
                "same-issued native request remained Busy at fixture deadline"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    #[tokio::test]
    #[ignore = "requires explicitly built owned native helper and fixture child"]
    async fn native_framed_ensure_create_inventory_and_exact_end_use_real_daemons() {
        let fixture = NativeFixture::new(true);
        let (native, source) = fixture.backend();
        let backend = Arc::new(LostReply {
            native,
            first: AtomicBool::new(false),
            committed: tokio::sync::Notify::new(),
            release: Condvar::new(),
            paused: Mutex::new(false),
        });
        let (mut client, server) = fixture
            .connect(backend.clone(), source, &Budgets::default())
            .await;
        let cancel = CancellationToken::new();
        let directory = private_storage::private_directory(&fixture.root, false).unwrap();
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
            let operation = client.issue_operation().unwrap();
            // Explicit cfg(test)-only backend advertisement exercises native
            // effects without weakening production capability enforcement.
            let first =
                issued_request_after_busy(&mut client, &operation, body.clone(), &cancel).await;
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
            let lease_name = format!("host-daemon-{}.json", session.target.generation.get());
            let held_lease = private_storage::open_private_file(
                &directory,
                std::path::Path::new(&format!(
                    "host-daemon-{}.lock",
                    session.target.generation.get()
                )),
                false,
            )
            .unwrap();
            fs2::FileExt::lock_exclusive(&held_lease).unwrap();
            let journal_before = private_storage::read_private(
                &directory,
                std::path::Path::new("host-sessions.json"),
                4 * 1024 * 1024,
            )
            .unwrap();
            let lease_before =
                private_storage::read_private(&directory, std::path::Path::new(&lease_name), 4096)
                    .unwrap();
            let busy = client
                .request_issued(&operation, body.clone(), &cancel)
                .await
                .unwrap();
            let mut expected_busy = first.clone();
            expected_busy.body = Response::Refused {
                reason: Refusal::Busy,
            };
            assert_eq!(busy, expected_busy);
            let lookup = Request::Lookup {
                logical: creation.logical,
            };
            let lookup_operation = OperationId::new();
            let busy_lookup = client
                .request(lookup_operation.clone(), lookup.clone(), &cancel)
                .await
                .unwrap();
            assert_eq!(
                busy_lookup.body,
                Response::Refused {
                    reason: Refusal::Busy
                },
                "{busy_lookup:?}"
            );
            assert_eq!(
                private_storage::read_private(
                    &directory,
                    std::path::Path::new("host-sessions.json"),
                    4 * 1024 * 1024,
                )
                .unwrap(),
                journal_before
            );
            assert_eq!(
                private_storage::read_private(&directory, std::path::Path::new(&lease_name), 4096,)
                    .unwrap(),
                lease_before
            );
            drop(held_lease);
            let retried = issued_request_after_busy(&mut client, &operation, body, &cancel).await;
            assert_eq!(retried, first);
            let lookup_deadline = Instant::now() + Duration::from_secs(8);
            let found = loop {
                let response = client
                    .request(lookup_operation.clone(), lookup.clone(), &cancel)
                    .await
                    .unwrap();
                if !matches!(
                    response.body,
                    Response::Refused {
                        reason: Refusal::Busy
                    }
                ) {
                    break response;
                }
                assert!(
                    Instant::now() < lookup_deadline,
                    "same-operation native Lookup remained Busy at fixture deadline"
                );
                tokio::time::sleep(Duration::from_millis(25)).await;
            };
            assert!(
                matches!(
                    &found.body,
                    Response::Lookup {
                        result: LookupResult::Running { session: found }
                    } if found.target == session.target
                ),
                "{found:?}"
            );
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
            let operation = OperationId::new();
            let body = Request::ExplicitEnd {
                target: target.clone(),
            };
            let held_lease = private_storage::open_private_file(
                &directory,
                std::path::Path::new(&format!("host-daemon-{}.lock", target.generation.get())),
                false,
            )
            .unwrap();
            fs2::FileExt::lock_exclusive(&held_lease).unwrap();
            let before = fixture.checkpoint("host-sessions.json", 4 * 1024 * 1024);
            let lease_name = format!("host-daemon-{}.json", target.generation.get());
            let lease_before = fixture.checkpoint(&lease_name, 4096);
            assert_eq!(
                client
                    .request(operation.clone(), body.clone(), &cancel)
                    .await
                    .unwrap()
                    .body,
                Response::Refused {
                    reason: Refusal::Busy
                }
            );
            assert_eq!(
                fixture.checkpoint("host-sessions.json", 4 * 1024 * 1024),
                before
            );
            assert_eq!(fixture.checkpoint(&lease_name, 4096), lease_before);
            drop(held_lease);
            *backend.paused.lock().unwrap() = true;
            backend.first.store(true, Ordering::Release);
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                let request = client.request(operation.clone(), body.clone(), &cancel);
                tokio::pin!(request);
                tokio::select! {
                    response = &mut request => {
                        let response = response.unwrap();
                        assert_eq!(response.body, Response::Refused { reason: Refusal::Busy });
                        assert!(Instant::now() < deadline, "{response:?}");
                        tokio::task::yield_now().await;
                    }
                    _ = backend.committed.notified() => {
                        let committed = fixture.checkpoint("host-sessions.json", 4 * 1024 * 1024);
                        let before_record = &before["sessions"][target.logical.as_str()];
                        let committed_record = &committed["sessions"][target.logical.as_str()];
                        assert_eq!(committed_record["phase"], "Ending");
                        assert_eq!(committed_record["generation"], before_record["generation"]);
                        assert_eq!(committed_record["identity"], before_record["identity"]);
                        assert_eq!(committed_record["epoch"].as_u64().unwrap(), before_record["epoch"].as_u64().unwrap() + 1);
                        assert_eq!(committed["operations"], before["operations"]);
                        assert_eq!(committed["issuance_epoch"], before["issuance_epoch"]);
                        let stopped = fixture.checkpoint(&lease_name, 4096);
                        assert_eq!(stopped["stopped"], true);
                        assert_eq!(stopped["native_pid"], lease_before["native_pid"]);
                        assert_eq!(stopped["generation"], lease_before["generation"]);
                        assert_eq!(stopped["writer_epoch"], lease_before["writer_epoch"]);
                        *backend.paused.lock().unwrap() = false;
                        backend.release.notify_all();
                        let response = request.await.unwrap();
                        assert_eq!(response.operation, operation);
                        assert_eq!(response.body, Response::Refused { reason: Refusal::OutcomeUncertain });
                        break;
                    }
                }
            }
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
        let operation = client.issue_operation().unwrap();
        assert_eq!(
            client
                .request_issued(
                    &operation,
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
            let committed = matches!(
                (&request.body, &result),
                (Request::Ensure { .. }, Ok(Response::Ensured { .. }))
                    | (Request::ExplicitEnd { .. }, Err(Refusal::OutcomeUncertain))
            );
            if committed && self.first.swap(false, Ordering::AcqRel) {
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
    async fn native_production_inventory_only_refuses_unadvertised_creation() {
        let fixture = NativeFixture::new(true);
        let (_, _, source) = verified(&fixture.policy);
        let backend = Arc::new(native::NativeControl::new(
            fixture.control(),
            source.clone(),
        ));
        let (mut stream, server) =
            raw_connection(&fixture.policy, backend, source, &Budgets::default()).await;
        assert_eq!(
            raw_request(
                &mut stream,
                &fixture.policy,
                Request::Ensure {
                    creation: fixture.creation()
                }
            )
            .await
            .body,
            Response::Refused {
                reason: Refusal::Unsupported
            }
        );
        assert!(festerm_sessiond::list_running_sessions_in(&fixture.root)
            .unwrap()
            .is_empty());
        drop(stream);
        assert!(server.await.unwrap().is_err());
    }

    #[tokio::test]
    #[ignore = "requires explicitly built owned native helper and fixture child"]
    async fn native_committed_creation_policy_contention_is_uncertain_not_disconnect() {
        fn public_response_kind(response: &Response) -> (&'static str, Option<Refusal>) {
            match response {
                Response::Capabilities { .. } => ("capabilities", None),
                Response::Inventory { .. } => ("inventory", None),
                Response::Profiles { .. } => ("profiles", None),
                Response::Lookup { .. } => ("lookup", None),
                Response::Ensured { .. } => ("ensured", None),
                Response::Attached { .. } => ("attached", None),
                Response::Detached { .. } => ("detached", None),
                Response::Ended { .. } => ("ended", None),
                Response::Refused { reason } => ("refused", Some(*reason)),
            }
        }

        for restore_pending_checkpoint in [false, true] {
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
            let (mut second, second_server) = fixture
                .connect(backend.clone(), source.clone(), &budgets)
                .await;
            let creation = fixture.creation();
            let operation = first.issue_operation().unwrap();
            let cancel = CancellationToken::new();
            let initial_identity = if restore_pending_checkpoint {
                let setup_deadline = Instant::now() + Duration::from_secs(8);
                let running = loop {
                    match fixture.control().ensure(
                        &fixture.policy.principal,
                        operation.operation(),
                        &creation,
                        false,
                        &AtomicBool::new(false),
                        |_| Ok::<_, ControlError>(()),
                    ) {
                        Ok(running) => break running,
                        Err(
                            ControlError::Busy
                            | ControlError::Unknown
                            | ControlError::CommittedButNotPublished,
                        ) if Instant::now() < setup_deadline => {
                            tokio::time::sleep(Duration::from_millis(25)).await;
                        }
                        Err(error) => panic!("owned creation setup failed: {error:?}"),
                    }
                };
                let mut journal = fixture.checkpoint("host-sessions.json", 4 * 1024 * 1024);
                let record = &mut journal["sessions"][creation.logical.as_str()];
                record["phase"] = "Launching".into();
                record["identity"] = serde_json::Value::Null;
                let directory = private_storage::private_directory(&fixture.root, false).unwrap();
                private_storage::replace_private(
                    &directory,
                    std::path::Path::new("host-sessions.json"),
                    &serde_json::to_vec(&journal).unwrap(),
                )
                .unwrap();
                assert_eq!(
                    first
                        .request_issued(
                            &operation,
                            Request::Ensure {
                                creation: creation.clone()
                            },
                            &cancel
                        )
                        .await
                        .unwrap()
                        .body,
                    Response::Refused {
                        reason: Refusal::Stale
                    },
                );
                Some((running.target.generation, running.identity.pid))
            } else {
                None
            };
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                // Reconcile a still-launching committed generation before replaying
                // the original operation; readiness never grants another creation.
                loop {
                    let response = second
                        .request(OperationId::new(), Request::List { cursor: None }, &cancel)
                        .await
                        .unwrap();
                    if matches!(response.body, Response::Inventory { .. }) {
                        break;
                    }
                    let (variant, refusal_code) = public_response_kind(&response.body);
                    assert!(
                        matches!(
                            response.body,
                            Response::Refused {
                                reason: Refusal::Busy | Refusal::Unknown | Refusal::Stale
                            }
                        ),
                        "creation readiness response variant={variant}, refusal_code={refusal_code:?}"
                    );
                    assert!(
                        Instant::now() < deadline,
                        "creation readiness response variant={variant}, refusal_code={refusal_code:?}"
                    );
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                let request = first.request_issued(
                    &operation,
                    Request::Ensure {
                        creation: creation.clone(),
                    },
                    &cancel,
                );
                tokio::pin!(request);
                tokio::select! {
                    result = &mut request => {
                        let response = result.unwrap();
                        let (variant, refusal_code) = public_response_kind(&response.body);
                        assert!(matches!(response.body, Response::Refused { reason: Refusal::Busy | Refusal::Unknown | Refusal::OutcomeUncertain }), "first Ensure response variant={variant}, refusal_code={refusal_code:?}");
                        assert!(Instant::now() < deadline, "first Ensure response variant={variant}, refusal_code={refusal_code:?}");
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                    _ = backend.committed.notified() => {
                        let held = source.0.clone().try_lock_owned().unwrap();
                        *backend.paused.lock().unwrap() = false;
                        backend.release.notify_all();
                        assert_eq!(request.await.unwrap().body, Response::Refused { reason: Refusal::OutcomeUncertain });
                        assert_eq!(second.request(OperationId::new(), Request::List { cursor: None }, &cancel).await.unwrap().body,
                            Response::Refused { reason: Refusal::Busy });
                        drop(held);
                        break;
                    }
                }
            }
            let journal = fixture.checkpoint("host-sessions.json", 4 * 1024 * 1024);
            let generation: GenerationId = serde_json::from_value(
                journal["sessions"][creation.logical.as_str()]["generation"].clone(),
            )
            .unwrap();
            let directory = private_storage::private_directory(&fixture.root, false).unwrap();
            let held_lease = private_storage::open_private_file(
                &directory,
                std::path::Path::new(&format!("host-daemon-{}.lock", generation.get())),
                false,
            )
            .unwrap();
            fs2::FileExt::lock_exclusive(&held_lease).unwrap();
            let lease_name = format!("host-daemon-{}.json", generation.get());
            let lease = fixture.checkpoint(&lease_name, 16 * 1024);
            if let Some((expected_generation, expected_pid)) = initial_identity {
                assert_eq!(generation, expected_generation);
                assert_eq!(lease["native_pid"], serde_json::json!(expected_pid));
            }
            assert_eq!(journal["sessions"].as_object().unwrap().len(), 1);
            assert_eq!(journal["operations"].as_object().unwrap().len(), 1);
            let body = Request::Ensure {
                creation: creation.clone(),
            };
            let response = first
                .request_issued(&operation, body.clone(), &cancel)
                .await
                .unwrap();
            let (variant, refusal_code) = public_response_kind(&response.body);
            assert!(
            matches!(
                response.body,
                Response::Refused {
                    reason: Refusal::Busy
                }
            ),
            "original-operation lease replay response variant={variant}, refusal_code={refusal_code:?}"
        );
            assert_eq!(
                fixture.checkpoint("host-sessions.json", 4 * 1024 * 1024),
                journal
            );
            assert_eq!(fixture.checkpoint(&lease_name, 16 * 1024), lease);
            drop(held_lease);
            loop {
                let response = first
                    .request_issued(&operation, body.clone(), &cancel)
                    .await
                    .unwrap();
                let (variant, refusal_code) = public_response_kind(&response.body);
                assert!(Instant::now() < deadline, "original-operation replay response variant={variant}, refusal_code={refusal_code:?}");
                match &response.body {
                Response::Ensured {
                    session,
                    disposition,
                    ..
                } => {
                    assert_eq!(session.target.logical, creation.logical);
                    assert_eq!(session.target.generation, generation);
                    assert_eq!(*disposition, EnsureDisposition::Created);
                    break;
                }
                Response::Refused {
                    reason: Refusal::Busy,
                } => {
                    assert_eq!(
                        fixture.checkpoint("host-sessions.json", 4 * 1024 * 1024),
                        journal,
                        "original-operation replay response variant={variant}, refusal_code={refusal_code:?}"
                    );
                    tokio::task::yield_now().await;
                }
                _ => panic!("committed original-operation replay variant={variant}, refusal_code={refusal_code:?}"),
            }
            }
            assert_eq!(
                fixture.checkpoint("host-sessions.json", 4 * 1024 * 1024),
                journal
            );
            assert_eq!(fixture.checkpoint(&lease_name, 16 * 1024), lease);
            assert_eq!(
                festerm_sessiond::list_running_sessions_in(&fixture.root)
                    .unwrap()
                    .len(),
                1
            );
            drop((first, second));
            assert!(first_server.await.unwrap().is_err());
            assert!(second_server.await.unwrap().is_err());
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
        let operation = first.issue_operation().unwrap();
        let cancel = CancellationToken::new();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let request = first.request_issued(
                &operation,
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
        let (mut second, second_server) = fixture
            .connect(backend.clone(), source.clone(), &budgets)
            .await;
        let held_policy = source.0.clone().lock_owned().await;
        assert_eq!(
            second
                .request_issued(
                    &operation,
                    Request::Ensure {
                        creation: creation.clone(),
                    },
                    &cancel,
                )
                .await
                .unwrap()
                .body,
            Response::Refused {
                reason: Refusal::Busy
            }
        );
        drop(held_policy);
        let retry_deadline = Instant::now() + Duration::from_secs(8);
        let result = loop {
            let result = second
                .request_issued(
                    &operation,
                    Request::Ensure {
                        creation: creation.clone(),
                    },
                    &cancel,
                )
                .await
                .unwrap();
            if matches!(
                result.body,
                Response::Refused {
                    reason: Refusal::Busy
                }
            ) && Instant::now() < retry_deadline
            {
                tokio::time::sleep(Duration::from_millis(25)).await;
            } else {
                break result;
            }
        };
        let Response::Ensured { session, .. } = result.body else {
            panic!("{result:?}")
        };
        assert_eq!(session.target.logical, creation.logical);
        let inventory_operation = OperationId::new();
        let inventory_deadline = Instant::now() + Duration::from_secs(8);
        let inventory = loop {
            let inventory = second
                .request(
                    inventory_operation.clone(),
                    Request::List { cursor: None },
                    &cancel,
                )
                .await
                .unwrap();
            if matches!(
                inventory.body,
                Response::Refused {
                    reason: Refusal::Busy
                }
            ) && Instant::now() < inventory_deadline
            {
                tokio::time::sleep(Duration::from_millis(25)).await;
            } else {
                break inventory;
            }
        };
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
