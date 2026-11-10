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

struct Policy(Mutex<OwnerVerifier>);
impl AuthorizationSource for Policy {
    fn with_current(
        &self,
        check: &mut dyn FnMut(&OwnerVerifier) -> Result<(), ContractError>,
    ) -> Result<(), ContractError> {
        let current = self.0.lock().map_err(|_| ContractError::Denied)?;
        check(&current)
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
    (owner, guard, Arc::new(Policy(Mutex::new(verifier))))
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
        .lock()
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
