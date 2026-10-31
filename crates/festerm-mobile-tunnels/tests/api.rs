//! Offline API tests: fake GitHub, fixed tunnel directory and a scripted bridge
//! host over in-memory streams. The real-daemon loopback lives in
//! `festerm-sessiond/tests/mobile_tunnel_loopback.rs`.
use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use festerm_core::{Dimensions, Terminal};
use festerm_mobile_profiles::{
    DevTunnelProfile, ProfileId, ProfileKind, ProfileStore, TunnelSource,
};
use festerm_mobile_tunnels::{
    testing::{
        seed_account, FakeGithub, FixedDirectory, StreamConnector, CLIENT_ID, OTHER_HOST_KEY,
    },
    CancelToken, HostTrust, SessionState, SignInState, TunnelClient, TunnelError,
};
use festerm_secret_store::MemorySecretStore;
use festerm_session::{noop_session_event_notifier, SessionEvent, SessionLifecycle};
use festerm_tunnel_bridge::{
    mobile::{BoxStream, Description, Displacer, Target},
    protocol::{
        encode_snapshot, FileAccess, Hello, ADOPTED, CAPABILITIES, DESCRIPTION, DETACH, DISCOVER,
        DISPLACED, HELLO, INPUT, SNAPSHOT, VERSION,
    },
    wire::{read_frame, write_frame},
};

struct Script {
    attached_by: Option<Displacer>,
    takeover_allowed: bool,
    hellos: Mutex<Vec<bool>>,
    connections: AtomicUsize,
    detached: AtomicBool,
    displace: tokio::sync::Notify,
    received_input: Mutex<Vec<u8>>,
}

fn script(attached_by: Option<Displacer>, takeover_allowed: bool) -> Arc<Script> {
    Arc::new(Script {
        attached_by,
        takeover_allowed,
        hellos: Mutex::new(Vec::new()),
        connections: AtomicUsize::new(0),
        detached: AtomicBool::new(false),
        displace: tokio::sync::Notify::new(),
        received_input: Mutex::new(Vec::new()),
    })
}

fn target() -> Target {
    Target {
        name: "main".into(),
        pid: 7,
        generation: "9".into(),
    }
}

async fn host(mut stream: tokio::io::DuplexStream, script: Arc<Script>) {
    let Ok((kind, bytes)) = read_frame(&mut stream).await else {
        return;
    };
    if kind == DISCOVER {
        let description = Description {
            version: VERSION,
            target: target(),
            files: FileAccess::default(),
            attached_by: script.attached_by,
            takeover_allowed: script.takeover_allowed,
        };
        let _ = write_frame(
            &mut stream,
            DESCRIPTION,
            &serde_json::to_vec(&description).unwrap(),
        )
        .await;
        return;
    }
    assert_eq!(kind, HELLO);
    let hello: Hello = serde_json::from_slice(&bytes).unwrap();
    script.hellos.lock().unwrap().push(hello.takeover);
    write_frame(&mut stream, CAPABILITIES, &[0, 0])
        .await
        .unwrap();
    let mut terminal = Terminal::new(Dimensions::new(80, 24).unwrap()).unwrap();
    terminal.ingest(b"recovered\r\n");
    write_frame(
        &mut stream,
        SNAPSHOT,
        &encode_snapshot(&terminal.recovery_clone()).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(read_frame(&mut stream).await.unwrap().0, ADOPTED);
    loop {
        tokio::select! {
            () = script.displace.notified() => {
                let _ = write_frame(&mut stream, DISPLACED, &[Displacer::Remote.code()]).await;
                return;
            }
            frame = read_frame(&mut stream) => match frame {
                Ok((DETACH, _)) => { script.detached.store(true, Ordering::SeqCst); return; }
                Ok((INPUT, bytes)) => script.received_input.lock().unwrap().extend(bytes),
                Ok(_) => {}
                Err(_) => return,
            },
        }
    }
}

struct Fixture {
    client: TunnelClient,
    store: Arc<Mutex<ProfileStore>>,
    github: Arc<FakeGithub>,
    directory: Arc<FixedDirectory>,
    script: Arc<Script>,
    account: String,
}

fn fixture(script: Arc<Script>, expired: bool) -> Fixture {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "tunnels-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    let store = Arc::new(Mutex::new(
        ProfileStore::open(&dir, Arc::new(MemorySecretStore::new())).unwrap(),
    ));
    let account = seed_account(&store, 42, "octo", expired);
    let github = Arc::new(FakeGithub::new(42, "octo"));
    let directory = Arc::new(FixedDirectory::new());
    let host_script = script.clone();
    let connector = StreamConnector(move || -> BoxStream {
        host_script.connections.fetch_add(1, Ordering::SeqCst);
        let (client, server) = tokio::io::duplex(1 << 16);
        tokio::spawn(host(server, host_script.clone()));
        Box::new(client)
    });
    let client = TunnelClient::with_parts(
        store.clone(),
        github.clone(),
        directory.clone(),
        Arc::new(connector),
        CLIENT_ID.into(),
    )
    .unwrap();
    Fixture {
        client,
        store,
        github,
        directory,
        script,
        account,
    }
}

fn profile(f: &Fixture, takeover: bool, trusted: bool) -> ProfileId {
    f.store
        .lock()
        .unwrap()
        .create(
            &format!("devbox-{takeover}-{trusted}"),
            ProfileKind::DevTunnel(DevTunnelProfile {
                account: Some(f.account.clone()),
                source: TunnelSource::Named {
                    tunnel_name: "devbox.euw".into(),
                    trusted_host: trusted.then(|| f.directory.trusted_host()),
                    connect_credential: None,
                },
                session: None,
                take_over_if_attached: takeover,
            }),
        )
        .unwrap()
}

fn wait_for(mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn device_sign_in_stores_the_bound_account_and_refuses_a_different_user() {
    let f = fixture(script(None, false), false);
    let cancel = CancelToken::new();
    let flow = f.client.begin_sign_in(None, &cancel).unwrap();
    assert_eq!(flow.prompt().user_code, "ABCD-1234");
    assert_eq!(
        flow.prompt().verification_url,
        "https://github.com/login/device"
    );
    assert!(flow.prompt().expires_in <= Duration::from_secs(900));
    assert_eq!(flow.state(), SignInState::Waiting);
    // The identity layer enforces the 5 s minimum polling interval.
    let SignInState::Done(account) = flow.wait(Duration::from_secs(20)) else {
        panic!("sign-in did not finish");
    };
    assert_eq!(account.login, "octo");
    assert_eq!(
        account.id, f.account,
        "the same numeric id re-signs the same account in"
    );
    assert_eq!(f.client.accounts().len(), 1);

    // Re-sign-in is bound to the stored numeric id: another GitHub user is refused.
    let client = TunnelClient::with_parts(
        f.store.clone(),
        Arc::new(FakeGithub::new(99, "mallory")),
        f.directory.clone(),
        Arc::new(StreamConnector(|| -> BoxStream { unreachable!() })),
        CLIENT_ID.into(),
    )
    .unwrap();
    let flow = client.begin_sign_in(Some(&f.account), &cancel).unwrap();
    assert_eq!(
        flow.wait(Duration::from_secs(20)),
        SignInState::Failed(TunnelError::AccountChanged)
    );
}

#[test]
fn sign_in_can_be_cancelled_while_waiting_for_the_user() {
    let f = fixture(script(None, false), false);
    let flow = f.client.begin_sign_in(None, &CancelToken::new()).unwrap();
    flow.cancel();
    assert_eq!(
        flow.wait(Duration::from_secs(3)),
        SignInState::Failed(TunnelError::Cancelled)
    );
    assert!(f.client.sign_out(&f.account).is_ok());
    assert!(f.client.accounts().is_empty());
}

#[test]
fn expired_credentials_are_refreshed_once_and_stored_in_place() {
    let f = fixture(script(None, false), true);
    let cancel = CancelToken::new();
    assert_eq!(
        f.client.verify_account(&f.account, &cancel).unwrap().login,
        "octo"
    );
    assert_eq!(f.github.refreshes.load(Ordering::Relaxed), 1);
    f.client.verify_account(&f.account, &cancel).unwrap();
    assert_eq!(
        f.github.refreshes.load(Ordering::Relaxed),
        1,
        "the refreshed credential replaced the stale one in the secret store"
    );
    assert_eq!(
        f.client.list_tunnels(&f.account, &cancel).unwrap()[0].locator,
        "devbox.euw"
    );
}

#[test]
fn describing_a_tunnel_reports_host_identity_trust_and_session_state() {
    let f = fixture(script(Some(Displacer::Local), true), false);
    let cancel = CancelToken::new();
    let description = f
        .client
        .describe_tunnel(&f.account, "devbox.euw", None, &cancel)
        .unwrap();
    assert_eq!(description.trust, HostTrust::New);
    assert!(description.fingerprint.starts_with("SHA256:"));
    assert_eq!(description.sessions[0].name, "main");
    assert_eq!(
        description.sessions[0].state,
        SessionState::Attached {
            by: Some(Displacer::Local)
        }
    );
    assert!(description.sessions[0].takeover_allowed);
    let again = f
        .client
        .describe_tunnel(&f.account, "devbox.euw", Some(&description.host), &cancel)
        .unwrap();
    assert_eq!(again.trust, HostTrust::Trusted);
    *f.directory.host_key.lock().unwrap() = OTHER_HOST_KEY.into();
    let changed = f
        .client
        .describe_tunnel(&f.account, "devbox.euw", Some(&description.host), &cancel)
        .unwrap();
    assert!(matches!(changed.trust, HostTrust::Changed { .. }));
    assert_eq!(
        f.client
            .describe_tunnel(&f.account, "missing", None, &cancel)
            .unwrap_err(),
        TunnelError::TunnelNotFound
    );
}

#[test]
fn attach_requires_an_approved_unchanged_host() {
    let f = fixture(script(None, true), false);
    let cancel = CancelToken::new();
    let id = profile(&f, true, false);
    let notifier = noop_session_event_notifier();
    let Err(TunnelError::HostNotTrusted { fingerprint }) =
        f.client.attach(&id, notifier.clone(), &cancel)
    else {
        panic!("an unapproved host must not be attached");
    };
    assert!(fingerprint.starts_with("SHA256:"));
    assert_eq!(f.script.connections.load(Ordering::SeqCst), 0);

    f.client
        .approve_host(&id, f.directory.trusted_host())
        .unwrap();
    assert!(f.client.trusted_host_for(&id).is_some());
    assert_eq!(
        f.client.list_sessions(&id, &cancel).unwrap()[0].state,
        SessionState::Unattached
    );
    let first = f.client.attach(&id, notifier.clone(), &cancel).unwrap();
    first.take_recovered_terminal().unwrap();
    first.detach(Duration::from_secs(3)).unwrap();
    wait_for(|| f.script.detached.load(Ordering::SeqCst));

    *f.directory.host_key.lock().unwrap() = OTHER_HOST_KEY.into();
    assert!(matches!(
        f.client.attach(&id, notifier, &cancel),
        Err(TunnelError::HostChanged { .. })
    ));
}

#[test]
fn takeover_follows_the_profile_and_displacement_is_never_silently_reattached() {
    let f = fixture(script(Some(Displacer::Local), true), false);
    let cancel = CancelToken::new();
    let notifier = noop_session_event_notifier();

    let refusing = profile(&f, false, true);
    assert_eq!(
        f.client.attach(&refusing, notifier.clone(), &cancel).err(),
        Some(TunnelError::SessionInUse {
            by: Some(Displacer::Local)
        })
    );
    assert!(
        f.script.hellos.lock().unwrap().is_empty(),
        "no hello without takeover consent"
    );

    let taking = profile(&f, true, true);
    let attachment = f.client.attach(&taking, notifier, &cancel).unwrap();
    assert_eq!(attachment.took_over, Some(Some(Displacer::Local)));
    assert_eq!(*f.script.hellos.lock().unwrap(), vec![true]);
    let terminal = attachment.take_recovered_terminal().unwrap();
    assert!(terminal.row_text(0).unwrap().starts_with("recovered"));
    let session = attachment.session();
    assert!(session.terminal_replies_owned_by_backend());
    wait_for(|| matches!(session.lifecycle(), SessionLifecycle::Running));
    session.try_send_input(b"ls\r").unwrap();
    wait_for(|| f.script.received_input.lock().unwrap().as_slice() == b"ls\r");

    let connections = f.script.connections.load(Ordering::SeqCst);
    f.script.displace.notify_one();
    wait_for(|| attachment.end_error().is_some());
    assert_eq!(
        attachment.end_error(),
        Some(TunnelError::Displaced(Displacer::Remote))
    );
    assert!(matches!(
        session.lifecycle(),
        SessionLifecycle::Disconnected(_)
    ));
    while let Ok(event) = session.try_recv_event() {
        assert!(!matches!(event, SessionEvent::Output(_)));
    }
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        f.script.connections.load(Ordering::SeqCst),
        connections,
        "a displaced session must not reconnect on its own"
    );
}

#[test]
fn a_pinned_session_name_that_the_host_does_not_serve_is_reported() {
    let f = fixture(script(None, false), false);
    let id = profile(&f, true, true);
    {
        let mut store = f.store.lock().unwrap();
        let current = store.get(&id).unwrap().clone();
        let ProfileKind::DevTunnel(mut dev) = current.kind else {
            unreachable!()
        };
        dev.session = Some("other".into());
        store
            .update(&id, &current.name, ProfileKind::DevTunnel(dev))
            .unwrap();
    }
    assert_eq!(
        f.client
            .attach(&id, noop_session_event_notifier(), &CancelToken::new())
            .err(),
        Some(TunnelError::SessionNotFound)
    );
}
