//! Linux virtual serial-port integration and failure tests.
//!
//! `K13 Serial` (`docs/gui-action-graph.md`) calls for "a future virtual
//! loopback or representative adapter fixture" as native-smoke evidence for
//! the serial backend, alongside `docs/manual-validation.md`'s CP-04 row. The
//! ordinary CI cases use `serialport::TTYPort::pair`, requiring no hardware or
//! external binary. The opt-in native smoke retains `socat` to cross-connect
//! two independently opened pseudo-terminal endpoints like a null-modem cable.
//!
//! Successful pseudo-terminal sessions are **Linux only**. The `serialport`
//! crate's own source
//! (`posix/termios.rs`) documents that setting a baud rate on a
//! pseudo-terminal on macOS always fails with `ENOTTY`, because macOS
//! unconditionally uses the `IOSSIOSPEED` ioctl, which pseudo-terminals do
//! not support (verified directly against this crate version while writing
//! this test). Linux's standard `termios` baud-rate path has no such
//! restriction, so a `socat` pty pair is a faithful loopback there. macOS
//! and Windows have no equivalent virtual-loopback path with this crate;
//! their native/manual validation (`docs/manual-validation.md` CP-04)
//! requires a real adapter (or, on Windows, a third-party virtual COM-port
//! driver such as `com0com`) with TX/RX shorted.
//!
//! Run explicitly with:
//! `cargo test -p festerm-serial --test socat_loopback -- --ignored --nocapture`
#![cfg(unix)]

#[cfg(target_os = "linux")]
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use festerm_serial::{LineSettings, SerialSession};
#[cfg(target_os = "linux")]
use festerm_session::{
    Session, SessionError, SessionErrorKind, SessionEvent, SessionLifecycle,
    SessionTryReceiveError, ShutdownError, ShutdownResult,
};
use serialport::{SerialPort, TTYPort};

#[cfg(target_os = "linux")]
const READY_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(target_os = "linux")]
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(target_os = "linux")]
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
#[cfg(target_os = "linux")]
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[cfg(target_os = "linux")]
struct PtyPeer {
    port: TTYPort,
    device: String,
}

#[cfg(target_os = "linux")]
impl PtyPeer {
    fn start() -> Self {
        let (mut port, slave) = TTYPort::pair().expect("could not create a pseudo-terminal pair");
        let device = slave
            .name()
            .expect("the pseudo-terminal slave has a device path");
        drop(slave);
        port.set_timeout(TRANSFER_TIMEOUT)
            .expect("could not bound pseudo-terminal peer reads");
        Self { port, device }
    }

    fn settings(&self) -> LineSettings {
        LineSettings::with_defaults(self.device.clone())
            .expect("valid pseudo-terminal line settings")
    }

    fn open_session(&self) -> SerialSession {
        SerialSession::open(self.settings()).expect("could not open pseudo-terminal session")
    }

    fn read_exact(&mut self, length: usize) -> Vec<u8> {
        let mut bytes = vec![0_u8; length];
        self.port
            .read_exact(&mut bytes)
            .expect("session output reaches the pseudo-terminal peer");
        bytes
    }

    fn write_all(&mut self, bytes: &[u8]) {
        self.port
            .write_all(bytes)
            .and_then(|()| self.port.flush())
            .expect("pseudo-terminal peer input reaches the session");
    }
}

/// Owns the `socat` child for the duration of the test and guarantees it is
/// killed even if an assertion panics.
#[cfg(target_os = "linux")]
struct SocatLoopback {
    child: Child,
    port_a: PathBuf,
    port_b: PathBuf,
}

#[cfg(target_os = "linux")]
impl SocatLoopback {
    fn start() -> Option<Self> {
        if Command::new("socat")
            .arg("-V")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_err()
        {
            eprintln!("skipping: `socat` is not installed on this machine");
            return None;
        }

        let pid = std::process::id();
        let port_a = std::env::temp_dir().join(format!("festerm-serial-loop-a-{pid}"));
        let port_b = std::env::temp_dir().join(format!("festerm-serial-loop-b-{pid}"));
        let _ = std::fs::remove_file(&port_a);
        let _ = std::fs::remove_file(&port_b);

        let mut child = Command::new("socat")
            .arg("-d")
            .arg("-d")
            .arg(format!("pty,raw,echo=0,link={}", port_a.to_string_lossy()))
            .arg(format!("pty,raw,echo=0,link={}", port_b.to_string_lossy()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("could not start socat for the serial loopback fixture");

        let deadline = Instant::now() + READY_TIMEOUT;
        while Instant::now() < deadline {
            if port_a.exists() && port_b.exists() {
                return Some(Self {
                    child,
                    port_a,
                    port_b,
                });
            }
            if let Some(status) = child
                .try_wait()
                .expect("could not inspect the socat loopback process")
            {
                panic!("socat exited before creating the loopback endpoints: {status}");
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("socat did not create both loopback pseudo-terminal endpoints in time");
    }

    fn port_a(&self) -> &Path {
        &self.port_a
    }

    fn port_b(&self) -> &Path {
        &self.port_b
    }
}

#[cfg(target_os = "linux")]
impl Drop for SocatLoopback {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.port_a);
        let _ = std::fs::remove_file(&self.port_b);
    }
}

#[cfg(target_os = "linux")]
fn recv_output_exact(session: &SerialSession, length: usize, timeout: Duration) -> Vec<u8> {
    let deadline = Instant::now() + timeout;
    let mut collected = Vec::new();
    while collected.len() < length && Instant::now() < deadline {
        match session.try_recv_event() {
            Ok(SessionEvent::Output(bytes)) => {
                collected.extend(bytes);
            }
            Ok(_) => {}
            Err(SessionTryReceiveError::Empty) => thread::sleep(EVENT_POLL_INTERVAL),
            Err(SessionTryReceiveError::Closed) => break,
        }
    }
    collected
}

#[cfg(target_os = "linux")]
fn wait_for_failure(session: &SerialSession, timeout: Duration) -> SessionError {
    let deadline = Instant::now() + timeout;
    loop {
        match session.lifecycle() {
            SessionLifecycle::Failed(error) => return error,
            SessionLifecycle::Disconnected(error) => {
                panic!("serial peer loss must fail, not disconnect: {error}")
            }
            lifecycle if Instant::now() >= deadline => {
                panic!("serial session did not fail in time; lifecycle: {lifecycle:?}")
            }
            _ => thread::sleep(EVENT_POLL_INTERVAL),
        }
    }
}

#[cfg(target_os = "linux")]
fn ordered_frames(prefix: &str) -> Vec<Vec<u8>> {
    (0..32)
        .map(|sequence| format!("{prefix}-{sequence:02}\r\n").into_bytes())
        .collect()
}

#[test]
fn opening_an_exclusively_held_pseudo_terminal_reports_busy() {
    let (_master, mut held_port) =
        TTYPort::pair().expect("could not create a busy pseudo-terminal fixture");
    held_port
        .set_exclusive(true)
        .expect("could not exclusively hold the pseudo-terminal");
    let device = held_port
        .name()
        .expect("the pseudo-terminal slave has a device path");

    let settings =
        LineSettings::with_defaults(device.clone()).expect("valid pseudo-terminal line settings");
    let Err(error) = SerialSession::open(settings) else {
        panic!("opening an exclusively held serial device must fail");
    };
    let message = error.to_string();
    assert!(message.starts_with("could not open serial device"));
    assert!(message.contains(&device));
}

#[cfg(target_os = "linux")]
#[test]
fn serial_session_preserves_order_in_both_directions() {
    let mut peer = PtyPeer::start();
    let session = peer.open_session();

    let outbound_frames = ordered_frames("session");
    let outbound = outbound_frames.concat();
    for frame in outbound_frames {
        session
            .try_send_input(&frame)
            .expect("ordered session input fits the bounded queue");
    }
    assert_eq!(peer.read_exact(outbound.len()), outbound);

    let inbound_frames = ordered_frames("peer");
    let inbound = inbound_frames.concat();
    for frame in inbound_frames {
        peer.write_all(&frame);
    }
    assert_eq!(
        recv_output_exact(&session, inbound.len(), TRANSFER_TIMEOUT),
        inbound
    );

    assert_eq!(
        session.shutdown(SHUTDOWN_TIMEOUT),
        Ok(ShutdownResult::Stopped)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn shutdown_releases_the_device_for_a_bounded_reopen() {
    let mut peer = PtyPeer::start();
    let first = peer.open_session();
    let first_id = first.id();

    assert_eq!(
        first.shutdown(SHUTDOWN_TIMEOUT),
        Ok(ShutdownResult::Stopped)
    );
    assert_eq!(first.lifecycle(), SessionLifecycle::Stopped);

    let reopened = peer.open_session();
    assert_ne!(reopened.id(), first_id);

    reopened
        .try_send_input(b"after reopen")
        .expect("reopened session accepts input");
    assert_eq!(peer.read_exact(b"after reopen".len()), b"after reopen");

    peer.write_all(b"peer after reopen");
    assert_eq!(
        recv_output_exact(&reopened, b"peer after reopen".len(), TRANSFER_TIMEOUT),
        b"peer after reopen"
    );

    assert_eq!(
        reopened.shutdown(SHUTDOWN_TIMEOUT),
        Ok(ShutdownResult::Stopped)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn peer_disconnect_reports_one_output_failure_and_never_disconnected() {
    let peer = PtyPeer::start();
    let session = peer.open_session();
    drop(peer);

    let failure = wait_for_failure(&session, TRANSFER_TIMEOUT);
    assert_eq!(failure.kind(), SessionErrorKind::Output);
    assert!(
        failure.message().contains("closed") || failure.message().contains("read failed"),
        "unexpected peer-loss error: {failure}"
    );
    assert_eq!(session.metrics().error_count, 1);

    let mut saw_error_event = false;
    let mut saw_failed_lifecycle = false;
    loop {
        match session.try_recv_event() {
            Ok(SessionEvent::Error(error)) if error == failure => saw_error_event = true,
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Failed(error))) if error == failure => {
                saw_failed_lifecycle = true;
            }
            Ok(SessionEvent::Lifecycle(SessionLifecycle::Disconnected(error))) => {
                panic!("serial peer loss must not become reconnectable: {error}")
            }
            Ok(_) => {}
            Err(SessionTryReceiveError::Empty | SessionTryReceiveError::Closed) => break,
        }
    }
    assert!(saw_error_event);
    assert!(saw_failed_lifecycle);

    assert_eq!(
        session.shutdown(SHUTDOWN_TIMEOUT),
        Err(ShutdownError::Failed(failure))
    );
}

/// Proves `SerialSession` opens a real device and delivers bytes in both
/// directions across a virtual null-modem loopback, satisfying `K13 Serial`.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires the `socat` binary; run explicitly or via native-smoke.yml"]
fn serial_session_delivers_bytes_across_a_socat_loopback_pair() {
    let Some(loopback) = SocatLoopback::start() else {
        return;
    };

    let settings_a = LineSettings::with_defaults(loopback.port_a().to_string_lossy())
        .expect("valid loopback line settings");
    let settings_b = LineSettings::with_defaults(loopback.port_b().to_string_lossy())
        .expect("valid loopback line settings");

    let session_a = SerialSession::open(settings_a).expect("could not open loopback endpoint A");
    let session_b = SerialSession::open(settings_b).expect("could not open loopback endpoint B");

    session_a
        .try_send_input(b"hello from A")
        .expect("endpoint A accepts input");
    let received_by_b = recv_output_exact(&session_b, b"hello from A".len(), TRANSFER_TIMEOUT);
    assert_eq!(received_by_b, b"hello from A");

    session_b
        .try_send_input(b"hello from B")
        .expect("endpoint B accepts input");
    let received_by_a = recv_output_exact(&session_a, b"hello from B".len(), TRANSFER_TIMEOUT);
    assert_eq!(received_by_a, b"hello from B");

    assert_eq!(
        session_a.shutdown(SHUTDOWN_TIMEOUT),
        Ok(ShutdownResult::Stopped)
    );
    assert_eq!(
        session_b.shutdown(SHUTDOWN_TIMEOUT),
        Ok(ShutdownResult::Stopped)
    );
}
