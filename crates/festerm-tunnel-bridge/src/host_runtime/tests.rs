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
    assert!(!caps.attach && !caps.ensure && !caps.create);
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
    }
    assert!(matches!(
        control
            .request(OperationId::new(), Request::List { cursor: None }, &cancel)
            .await
            .unwrap()
            .body,
        Response::Inventory { .. }
    ));
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
