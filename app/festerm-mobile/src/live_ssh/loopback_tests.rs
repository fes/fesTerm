//! Real SSH protocol integration over an owned IPv4 loopback listener.
//! No OS shell, external host, account store or private credential is used.

use super::*;
use crate::{Lifecycle, MobileApp};
use egui_kittest::{kittest::Queryable, Harness};
use russh::server::{self, Server as _};
use russh::{Channel, ChannelId};
use std::{
    cell::Cell,
    net::{SocketAddr, TcpListener},
    rc::Rc,
    sync::Mutex,
    thread,
};
use tokio::sync::oneshot;

const USER: &str = "fixture-user";
const PASSWORD: &str = "loopback-only-test-password";
const EXPECTED_INPUT: &[u8] = b"probe\xce\xbb\r\x03\x1bOA";
const MAX_BYTES: usize = 4096;

#[derive(Default)]
struct Observed {
    password_attempts: usize,
    rejected_passwords: usize,
    shells: usize,
    retired: usize,
    input: Vec<u8>,
    sizes: Vec<(u32, u32)>,
    input_verified: bool,
}

struct FixtureServer(Arc<Mutex<Observed>>);
struct FixtureHandler(Arc<Mutex<Observed>>);

impl server::Server for FixtureServer {
    type Handler = FixtureHandler;

    fn new_client(&mut self, peer: Option<SocketAddr>) -> Self::Handler {
        assert!(peer.is_some_and(|peer| peer.ip().is_loopback()));
        FixtureHandler(self.0.clone())
    }
}

impl Drop for FixtureHandler {
    fn drop(&mut self) {
        self.0.lock().expect("fixture observations").retired += 1;
    }
}

impl server::Handler for FixtureHandler {
    type Error = russh::Error;

    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<server::Auth, Self::Error> {
        let mut observed = self.0.lock().expect("fixture observations");
        observed.password_attempts += 1;
        if user == USER && password == PASSWORD {
            Ok(server::Auth::Accept)
        } else {
            observed.rejected_passwords += 1;
            Ok(server::Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        columns: u32,
        rows: u32,
        _pixel_width: u32,
        _pixel_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        let mut observed = self.0.lock().expect("fixture observations");
        if observed.sizes.len() >= 16 {
            return Err(russh::Error::Disconnect);
        }
        observed.sizes.push((columns, rows));
        session.channel_success(channel)
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.0.lock().expect("fixture observations").shells += 1;
        session.channel_success(channel)?;
        // Application cursor mode and DSR exercise real shared-core encoding.
        session.data(
            channel,
            &b"\x1b[2J\x1b[HLOOPBACK SSH READY\r\n\x1b[?1h\x1b[6n"[..],
        )
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        bytes: &[u8],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        let verified = {
            let mut observed = self.0.lock().expect("fixture observations");
            if observed.input.len().saturating_add(bytes.len()) > MAX_BYTES {
                return Err(russh::Error::Disconnect);
            }
            observed.input.extend_from_slice(bytes);
            if !observed.input_verified && observed.input.ends_with(EXPECTED_INPUT) {
                observed.input_verified = true;
                true
            } else {
                false
            }
        };
        if verified {
            session.data(channel, &b"\r\nINPUT VERIFIED\r\n"[..])?;
        }
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        _channel: ChannelId,
        columns: u32,
        rows: u32,
        _pixel_width: u32,
        _pixel_height: u32,
        _session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        let mut observed = self.0.lock().expect("fixture observations");
        if observed.sizes.len() >= 16 {
            return Err(russh::Error::Disconnect);
        }
        observed.sizes.push((columns, rows));
        Ok(())
    }
}

struct Loopback {
    address: SocketAddr,
    observed: Arc<Mutex<Observed>>,
    stop: Option<oneshot::Sender<()>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Loopback {
    fn start() -> Self {
        // Keep ownership of the bound port while moving it into the runtime.
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind owned loopback");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let address = listener.local_addr().expect("owned listener address");
        let mut random = russh::keys::key::safe_rng();
        let key = russh::keys::PrivateKey::random(&mut random, russh::keys::Algorithm::Ed25519)
            .expect("ephemeral fixture host key");
        let config = Arc::new(server::Config {
            keys: vec![key],
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            inactivity_timeout: Some(Duration::from_secs(20)),
            ..Default::default()
        });
        let observed = Arc::new(Mutex::new(Observed::default()));
        let server_observed = observed.clone();
        let (stop, stopped) = oneshot::channel();
        let worker = thread::Builder::new()
            .name("festerm-mobile-ssh-fixture".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("fixture runtime");
                runtime.block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(listener)
                        .expect("fixture runtime listener");
                    let mut server = FixtureServer(server_observed);
                    let running = server.run_on_socket(config, &listener);
                    let handle = running.handle();
                    tokio::spawn(async move {
                        // The test guard normally stops immediately; also bound
                        // the fixture if a test abandons its guard unexpectedly.
                        let _ = tokio::time::timeout(Duration::from_secs(45), stopped).await;
                        handle.shutdown("controlled fixture complete".into());
                    });
                    running.await.expect("owned SSH fixture server");
                });
            })
            .expect("fixture thread");
        Self {
            address,
            observed,
            stop: Some(stop),
            worker: Some(worker),
        }
    }

    fn app(&self, changed_key: bool) -> MobileApp {
        let mut app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle {
            active: true,
            ..Default::default()
        })));
        app.editor.open_reconnect();
        app.editor.host = self.address.ip().to_string();
        app.editor.port = self.address.port().to_string();
        app.editor.username = USER.to_owned();
        if changed_key {
            app.connection.set_pin(
                &self.address.ip().to_string(),
                self.address.port(),
                "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            );
        }
        app
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !thread::panicking() {
                result.expect("owned SSH fixture stopped cleanly");
            }
        }
    }
}

fn until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "controlled SSH state deadline");
        thread::sleep(Duration::from_millis(5));
    }
}

fn visible_text(app: &MobileApp) -> String {
    (0..app.terminal.dimensions().rows())
        .filter_map(|row| app.terminal.row_text(row))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn mobile_loopback_ssh_connects_after_explicit_trust_and_password_retry() {
    let fixture = Loopback::start();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(390.0, 844.0))
        .build_ui_state(|ui, app: &mut MobileApp| app.show(ui), fixture.app(false));
    harness.run_steps(4);
    harness.get_by_label("Connect").click();
    harness.run_steps(4);
    until(|| {
        harness.step();
        harness
            .state()
            .sink
            .live
            .connection
            .borrow()
            .as_ref()
            .is_some_and(|live| live.host_prompt.is_some())
    });
    assert_eq!(fixture.observed.lock().unwrap().password_attempts, 0);
    assert_eq!(fixture.observed.lock().unwrap().shells, 0);
    harness.run_steps(4);
    harness.get_by_label("Trust & Connect").click();
    harness.run_steps(4);
    until(|| {
        harness.step();
        harness
            .state()
            .sink
            .live
            .connection
            .borrow()
            .as_ref()
            .is_some_and(|live| live.password_prompt.is_some())
    });
    assert_eq!(fixture.observed.lock().unwrap().password_attempts, 0);
    {
        let mut live = harness.state().sink.live.connection.borrow_mut();
        live.as_mut()
            .unwrap()
            .password
            .0
            .push_str("wrong-fixture-password");
    }
    harness.run_steps(4);
    harness.get_by_label("Authenticate").click();
    harness.run_steps(4);
    until(|| {
        harness.step();
        harness
            .state()
            .sink
            .live
            .connection
            .borrow()
            .as_ref()
            .and_then(|live| live.password_prompt.as_ref())
            .is_some_and(|prompt| prompt.previous_attempt_failed() && prompt.attempt() == 2)
    });
    assert_eq!(fixture.observed.lock().unwrap().rejected_passwords, 1);
    assert_eq!(fixture.observed.lock().unwrap().shells, 0);
    {
        let mut live = harness.state().sink.live.connection.borrow_mut();
        let live = live.as_mut().unwrap();
        assert!(live.password.0.is_empty());
        assert!(!live.input_enabled());
        live.password.0.push_str(PASSWORD);
    }
    harness.run_steps(4);
    harness.get_by_label("Authenticate").click();
    harness.run_steps(4);
    until(|| {
        harness.step();
        visible_text(harness.state()).contains("LOOPBACK SSH READY")
            && fixture.observed.lock().unwrap().input == b"\x1b[2;1R"
    });
    assert_eq!(fixture.observed.lock().unwrap().password_attempts, 2);
    assert_eq!(fixture.observed.lock().unwrap().shells, 1);
    {
        let live = harness.state().sink.live.connection.borrow();
        let live = live.as_ref().unwrap();
        assert!(live.input_enabled());
        assert!(live.accepted_pin.is_some());
        assert!(live.password.0.is_empty());
    }
    harness.event(egui::Event::Text("probe".into()));
    harness.run_steps(4);
    harness.event(egui::Event::Ime(egui::ImeEvent::Commit("λ".into())));
    harness.run_steps(4);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(4);
    harness.get_by_label("Ctrl").click();
    harness.run_steps(4);
    harness.event(egui::Event::Text("c".into()));
    harness.run_steps(4);
    harness.state_mut().send_key(festerm_core::Key::ArrowUp);
    until(|| {
        harness.step();
        visible_text(harness.state()).contains("INPUT VERIFIED")
    });
    let mut expected = b"\x1b[2;1R".to_vec();
    expected.extend_from_slice(EXPECTED_INPUT);
    assert_eq!(fixture.observed.lock().unwrap().input, expected);

    let initial = harness.state().terminal.dimensions();
    harness.set_size(egui::vec2(834.0, 1194.0));
    until(|| {
        harness.step();
        let size = harness.state().terminal.dimensions();
        size != initial
            && fixture
                .observed
                .lock()
                .unwrap()
                .sizes
                .contains(&(size.columns() as u32, size.rows() as u32))
    });
    harness.get_by_label("More").click();
    harness.run_steps(4);
    harness.get_by_label("Disconnect").click();
    harness.run_steps(4);
    until(|| {
        harness.step();
        fixture.observed.lock().unwrap().retired == 1
    });
    let count = fixture.observed.lock().unwrap().input.len();
    harness.event(egui::Event::Text("never replay".into()));
    harness.event(egui::Event::Paste("never paste\n".into()));
    harness.run_steps(4);
    assert_eq!(fixture.observed.lock().unwrap().input.len(), count);
    let live = harness.state().sink.live.connection.borrow();
    let live = live.as_ref().unwrap();
    assert!(live.can_reset());
    assert!(!live.input_enabled());
    assert!(live.host_prompt.is_none() && live.password_prompt.is_none());
    assert!(live.password.0.is_empty());
}

#[test]
fn mobile_loopback_ssh_rejects_unknown_and_changed_hosts_before_authentication() {
    for changed_key in [false, true] {
        let fixture = Loopback::start();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(
                |ui, app: &mut MobileApp| app.show(ui),
                fixture.app(changed_key),
            );
        harness.run_steps(4);
        harness.get_by_label("Connect").click();
        harness.run_steps(4);
        until(|| {
            harness.step();
            harness
                .state()
                .sink
                .live
                .connection
                .borrow()
                .as_ref()
                .is_some_and(|live| live.host_prompt.is_some())
        });
        {
            let live = harness.state().sink.live.connection.borrow();
            let live = live.as_ref().unwrap();
            assert_eq!(
                live.host_prompt.as_ref().unwrap().is_key_change(),
                changed_key
            );
            assert!(live.password_prompt.is_none());
        }
        assert_eq!(
            harness.query_by_label("Trust & Connect").is_none(),
            changed_key
        );
        harness.run_steps(4);
        harness.get_by_label("Cancel").click();
        harness.run_steps(4);
        until(|| {
            harness.step();
            fixture.observed.lock().unwrap().retired == 1
        });
        let observed = fixture.observed.lock().unwrap();
        assert_eq!(observed.password_attempts, 0);
        assert_eq!(observed.shells, 0);
        assert!(observed.input.is_empty());
        until(|| {
            harness.step();
            harness
                .state()
                .sink
                .live
                .connection
                .borrow()
                .as_ref()
                .is_some_and(|live| live.can_reset())
        });
    }
}
