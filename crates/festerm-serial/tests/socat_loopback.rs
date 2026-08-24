//! Native `socat`-backed loopback smoke test.
//!
//! `K13 Serial` (`docs/gui-action-graph.md`) calls for "a future virtual
//! loopback or representative adapter fixture" as native-smoke evidence for
//! the serial backend, alongside `docs/manual-validation.md`'s CP-04 row.
//! This test provides that virtual-loopback evidence using `socat` to
//! cross-connect two pseudo-terminal endpoints, exactly the way real serial
//! cabling would loop two ports together; it proves `SerialSession` actually
//! opens a device, and that bytes written on one session are read back,
//! byte-for-byte, from the other.
//!
//! **Linux only.** The `serialport` crate's own source
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
#![cfg(target_os = "linux")]

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use festerm_serial::{LineSettings, SerialSession};
use festerm_session::{Session, SessionEvent, SessionTryReceiveError};

const READY_TIMEOUT: Duration = Duration::from_secs(5);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(5);

/// Owns the `socat` child for the duration of the test and guarantees it is
/// killed even if an assertion panics.
struct SocatLoopback {
    child: Child,
    port_a: PathBuf,
    port_b: PathBuf,
}

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

        let child = Command::new("socat")
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
                // socat's symlinks can briefly exist before both pty sides
                // are fully wired; a short settle avoids a racy first open.
                thread::sleep(Duration::from_millis(200));
                return Some(Self {
                    child,
                    port_a,
                    port_b,
                });
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!("socat did not create both loopback pseudo-terminal endpoints in time");
    }

    fn port_a(&self) -> &Path {
        &self.port_a
    }

    fn port_b(&self) -> &Path {
        &self.port_b
    }
}

impl Drop for SocatLoopback {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.port_a);
        let _ = std::fs::remove_file(&self.port_b);
    }
}

fn recv_output(session: &SerialSession, timeout: Duration) -> Vec<u8> {
    let deadline = Instant::now() + timeout;
    let mut collected = Vec::new();
    while Instant::now() < deadline {
        match session.try_recv_event() {
            Ok(SessionEvent::Output(bytes)) => {
                collected.extend(bytes);
                if !collected.is_empty() {
                    return collected;
                }
            }
            Ok(_) => {}
            Err(SessionTryReceiveError::Empty) => thread::sleep(Duration::from_millis(20)),
            Err(SessionTryReceiveError::Closed) => break,
        }
    }
    collected
}

/// Proves `SerialSession` opens a real device and delivers bytes in both
/// directions across a virtual null-modem loopback, satisfying `K13 Serial`.
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
    let received_by_b = recv_output(&session_b, TRANSFER_TIMEOUT);
    assert_eq!(received_by_b, b"hello from A");

    session_b
        .try_send_input(b"hello from B")
        .expect("endpoint B accepts input");
    let received_by_a = recv_output(&session_a, TRANSFER_TIMEOUT);
    assert_eq!(received_by_a, b"hello from B");

    session_a
        .shutdown(Duration::from_secs(2))
        .expect("endpoint A shuts down within the bounded timeout");
    session_b
        .shutdown(Duration::from_secs(2))
        .expect("endpoint B shuts down within the bounded timeout");
}
