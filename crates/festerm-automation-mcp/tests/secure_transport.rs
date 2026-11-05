use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use festerm_automation_mcp::secure_transport::{
    Error, NativeCleanupFailed, NativeDecision, NativeUnavailable, Progress, Role, SecureTransport,
    TrustedNativeVerification, MAX_BOOTSTRAP_DURATION, MAX_FRAME_DURATION,
};

#[derive(Default)]
struct Wire {
    incoming: [VecDeque<u8>; 2],
    written: [Vec<u8>; 2],
    open: [bool; 2],
    timeout_write: [bool; 2],
    panic_write: [bool; 2],
    write_delay: [Duration; 2],
}

struct TestStream {
    side: usize,
    wire: Arc<Mutex<Wire>>,
    blocked: Arc<AtomicBool>,
    chunk: usize,
    interruptions: bool,
    reads: usize,
    writes: usize,
}

impl Read for TestStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.reads += 1;
        if self.interruptions && self.reads % 5 == 1 {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let mut wire = self.wire.lock().unwrap();
        if wire.incoming[self.side].is_empty() {
            return if wire.open[1 - self.side] {
                Err(io::ErrorKind::WouldBlock.into())
            } else {
                Ok(0)
            };
        }
        let count = bytes
            .len()
            .min(self.chunk)
            .min(wire.incoming[self.side].len());
        for byte in &mut bytes[..count] {
            *byte = wire.incoming[self.side].pop_front().unwrap();
        }
        Ok(count)
    }
}

impl Write for TestStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        if self.interruptions && self.writes % 5 == 1 {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let (timeout, panic_write, delay) = {
            let wire = self.wire.lock().unwrap();
            (
                wire.timeout_write[self.side],
                wire.panic_write[self.side],
                wire.write_delay[self.side],
            )
        };
        assert!(!panic_write, "test physical writer");
        if timeout {
            return Err(io::ErrorKind::TimedOut.into());
        }
        if !delay.is_zero() {
            thread::sleep(delay);
        }
        if self.blocked.load(Ordering::Acquire) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let mut wire = self.wire.lock().unwrap();
        if !wire.open[1 - self.side] {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let count = bytes.len().min(self.chunk);
        wire.written[self.side].extend_from_slice(&bytes[..count]);
        wire.incoming[1 - self.side].extend(&bytes[..count]);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for TestStream {
    fn drop(&mut self) {
        self.wire.lock().unwrap().open[self.side] = false;
    }
}

struct Native {
    display: SyncSender<String>,
    decision: Receiver<NativeDecision>,
    cancelled: Arc<AtomicBool>,
    panic_decision: Arc<AtomicBool>,
    fail_cancel: Arc<AtomicBool>,
    prompt_active: Arc<AtomicBool>,
}

impl TrustedNativeVerification for Native {
    fn show_local_fingerprint(&mut self, fingerprint: &str) -> Result<(), NativeUnavailable> {
        self.display
            .send(fingerprint.to_owned())
            .map_err(|_| NativeUnavailable)?;
        self.prompt_active.store(true, Ordering::Release);
        Ok(())
    }

    fn poll_decision(&mut self) -> NativeDecision {
        assert!(
            !self.panic_decision.swap(false, Ordering::AcqRel),
            "test native callback"
        );
        match self.decision.try_recv() {
            Ok(decision) => decision,
            Err(mpsc::TryRecvError::Empty) => NativeDecision::Pending,
            Err(mpsc::TryRecvError::Disconnected) => NativeDecision::Denied,
        }
    }

    fn cancel(&mut self) -> Result<(), NativeCleanupFailed> {
        if self.fail_cancel.load(Ordering::Acquire) {
            return Err(NativeCleanupFailed);
        }
        self.cancelled.store(true, Ordering::Release);
        self.prompt_active.store(false, Ordering::Release);
        Ok(())
    }
}

type Endpoint = SecureTransport<TestStream, Native>;

struct Pair {
    adapter: Endpoint,
    app: Endpoint,
    adapter_display: Receiver<String>,
    app_display: Receiver<String>,
    adapter_decision: SyncSender<NativeDecision>,
    app_decision: SyncSender<NativeDecision>,
    adapter_blocked: Arc<AtomicBool>,
    app_blocked: Arc<AtomicBool>,
    adapter_cancelled: Arc<AtomicBool>,
    app_cancelled: Arc<AtomicBool>,
    adapter_panic: Arc<AtomicBool>,
    wire: Arc<Mutex<Wire>>,
}

fn pair(chunk: usize, interruptions: bool, bootstrap: Duration) -> Pair {
    pair_with_frame_duration(chunk, interruptions, bootstrap, MAX_FRAME_DURATION)
}

fn pair_with_frame_duration(
    chunk: usize,
    interruptions: bool,
    bootstrap: Duration,
    frame_duration: Duration,
) -> Pair {
    let wire = Arc::new(Mutex::new(Wire {
        open: [true, true],
        ..Wire::default()
    }));
    let adapter_blocked = Arc::new(AtomicBool::new(false));
    let app_blocked = Arc::new(AtomicBool::new(false));
    let adapter_cancelled = Arc::new(AtomicBool::new(false));
    let app_cancelled = Arc::new(AtomicBool::new(false));
    let adapter_panic = Arc::new(AtomicBool::new(false));
    let adapter_fail_cancel = Arc::new(AtomicBool::new(false));
    let adapter_prompt_active = Arc::new(AtomicBool::new(false));
    let stream = |side, blocked| TestStream {
        side,
        wire: wire.clone(),
        blocked,
        chunk,
        interruptions,
        reads: 0,
        writes: 0,
    };
    let (adapter_display_send, adapter_display) = mpsc::sync_channel(1);
    let (app_display_send, app_display) = mpsc::sync_channel(1);
    let (adapter_decision, adapter_decisions) = mpsc::sync_channel(1);
    let (app_decision, app_decisions) = mpsc::sync_channel(1);
    Pair {
        adapter: SecureTransport::new(
            stream(0, adapter_blocked.clone()),
            Role::Client,
            Native {
                display: adapter_display_send,
                decision: adapter_decisions,
                cancelled: adapter_cancelled.clone(),
                panic_decision: adapter_panic.clone(),
                fail_cancel: adapter_fail_cancel.clone(),
                prompt_active: adapter_prompt_active.clone(),
            },
            bootstrap,
            frame_duration,
        )
        .unwrap(),
        app: SecureTransport::new(
            stream(1, app_blocked.clone()),
            Role::Application,
            Native {
                display: app_display_send,
                decision: app_decisions,
                cancelled: app_cancelled.clone(),
                panic_decision: Arc::new(AtomicBool::new(false)),
                fail_cancel: Arc::new(AtomicBool::new(false)),
                prompt_active: Arc::new(AtomicBool::new(false)),
            },
            bootstrap,
            frame_duration,
        )
        .unwrap(),
        adapter_display,
        app_display,
        adapter_decision,
        app_decision,
        adapter_blocked,
        app_blocked,
        adapter_cancelled,
        app_cancelled,
        adapter_panic,
        wire,
    }
}

fn poll(endpoint: &mut Endpoint) -> Result<Progress, Error> {
    endpoint.poll(|| Ok(()))
}

fn displays(pair: &mut Pair) -> (String, String) {
    let mut adapter = None;
    let mut app = None;
    for _ in 0..20_000 {
        let _ = poll(&mut pair.adapter).unwrap();
        let _ = poll(&mut pair.app).unwrap();
        adapter = adapter.or_else(|| pair.adapter_display.try_recv().ok());
        app = app.or_else(|| pair.app_display.try_recv().ok());
        if let (Some(adapter), Some(app)) = (&adapter, &app) {
            return (adapter.clone(), app.clone());
        }
    }
    panic!("both independent displays must be reached");
}

fn compared(peer_display: &str) -> NativeDecision {
    NativeDecision::ComparedPeer(peer_display.as_bytes().try_into().unwrap())
}

fn approve_both(pair: &Pair, adapter_display: &str, app_display: &str) {
    pair.adapter_decision.send(compared(app_display)).unwrap();
    pair.app_decision.send(compared(adapter_display)).unwrap();
}

fn establish(pair: &mut Pair) {
    for _ in 0..20_000 {
        let _ = poll(&mut pair.adapter).unwrap();
        let _ = poll(&mut pair.app).unwrap();
        if pair.adapter.is_established() && pair.app.is_established() {
            return;
        }
    }
    panic!("mutual confirmations did not complete");
}

fn receive(sender: &mut Endpoint, receiver: &mut Endpoint, message: &[u8]) {
    sender
        .prepare_send(message, Instant::now() + MAX_FRAME_DURATION)
        .unwrap();
    let mut sent = false;
    let mut received = None;
    for _ in 0..30_000 {
        if matches!(poll(sender).unwrap(), Progress::Sent) {
            sent = true;
        }
        if let Progress::Frame(frame) = poll(receiver).unwrap() {
            received = Some(frame);
        }
        if sent && received.is_some() {
            assert_eq!(received.as_deref(), Some(message));
            return;
        }
    }
    panic!("authenticated frame did not complete");
}

#[test]
fn owned_transport_confirms_independently_and_encrypts_fragmented_duplex_frames() {
    let mut pair = pair(3, true, MAX_BOOTSTRAP_DURATION);
    let (adapter_display, app_display) = displays(&mut pair);
    assert_eq!(adapter_display.len(), 64);
    assert_eq!(adapter_display, app_display);
    approve_both(&pair, &adapter_display, &app_display);
    establish(&mut pair);
    let request = b"sensitive request never appears on the private wire";
    let response = b"terminal output never appears on the private wire";
    receive(&mut pair.adapter, &mut pair.app, request);
    receive(&mut pair.app, &mut pair.adapter, response);
    let wire = pair.wire.lock().unwrap();
    for physical in &wire.written {
        for sensitive in [
            request.as_slice(),
            response.as_slice(),
            adapter_display.as_bytes(),
        ] {
            assert!(!physical
                .windows(sensitive.len())
                .any(|part| part == sensitive));
        }
    }
}

#[test]
fn payload_is_withheld_until_both_native_decisions_and_receipts_complete() {
    let mut pair = pair(2, false, MAX_BOOTSTRAP_DURATION);
    let (adapter_display, app_display) = displays(&mut pair);
    assert_eq!(
        pair.adapter
            .prepare_send(b"private", Instant::now() + MAX_FRAME_DURATION),
        Err(Error::NotEstablished)
    );
    pair.adapter_decision.send(compared(&app_display)).unwrap();
    for _ in 0..500 {
        assert!(!matches!(
            poll(&mut pair.adapter).unwrap(),
            Progress::Frame(_)
        ));
        assert!(!matches!(poll(&mut pair.app).unwrap(), Progress::Frame(_)));
    }
    assert!(!pair.adapter.is_established());
    assert!(!pair.app.is_established());
    assert_eq!(
        pair.adapter
            .prepare_send(b"private", Instant::now() + MAX_FRAME_DURATION),
        Err(Error::NotEstablished)
    );

    pair.app_blocked.store(true, Ordering::Release);
    pair.app_decision.send(compared(&adapter_display)).unwrap();
    for _ in 0..500 {
        let _ = poll(&mut pair.adapter).unwrap();
        let _ = poll(&mut pair.app).unwrap();
    }
    assert!(!pair.adapter.is_established());
    assert!(!pair.app.is_established());
    pair.app_blocked.store(false, Ordering::Release);
    establish(&mut pair);
    receive(&mut pair.adapter, &mut pair.app, b"private");
}

#[test]
fn mismatch_denial_and_callback_panic_discard_the_owned_stream() {
    for mismatch in [false, true] {
        let mut pair = pair(8, false, MAX_BOOTSTRAP_DURATION);
        let (_, app_display) = displays(&mut pair);
        let decision = if mismatch {
            let mut wrong = app_display.into_bytes();
            wrong[0] = if wrong[0] == b'0' { b'1' } else { b'0' };
            NativeDecision::ComparedPeer(wrong.try_into().unwrap())
        } else {
            NativeDecision::Denied
        };
        pair.adapter_decision.send(decision).unwrap();
        assert_eq!(poll(&mut pair.adapter).unwrap_err(), Error::Denied);
        assert!(!pair.wire.lock().unwrap().open[0]);
        assert_eq!(poll(&mut pair.adapter).unwrap_err(), Error::Closed);
        assert_eq!(
            pair.adapter
                .prepare_send(b"private", Instant::now() + MAX_FRAME_DURATION),
            Err(Error::Closed)
        );
    }

    let mut pair = pair(8, false, MAX_BOOTSTRAP_DURATION);
    assert_eq!(
        pair.adapter
            .poll::<()>(|| panic!("native admission callback failed"))
            .unwrap_err(),
        Error::CallbackPanicked
    );
    assert!(!pair.wire.lock().unwrap().open[0]);
}

#[test]
fn old_encrypted_confirmation_and_spoofed_control_frames_are_rejected() {
    let mut old = pair(usize::MAX, false, MAX_BOOTSTRAP_DURATION);
    let (_, old_app_display) = displays(&mut old);
    old.adapter_decision
        .send(compared(&old_app_display))
        .unwrap();
    for _ in 0..100 {
        let _ = poll(&mut old.adapter).unwrap();
        if !old.adapter.has_pending_output() {
            break;
        }
    }
    let old_confirmation = {
        let wire = old.wire.lock().unwrap();
        assert!(wire.written[0].len() > 34);
        wire.written[0][34..].to_vec()
    };

    for injected in [
        old_confirmation,
        vec![
            0, 20, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42,
        ],
    ] {
        let mut fresh = pair(usize::MAX, false, MAX_BOOTSTRAP_DURATION);
        let (adapter_display, _) = displays(&mut fresh);
        fresh.app_decision.send(compared(&adapter_display)).unwrap();
        fresh.wire.lock().unwrap().incoming[1].extend(injected);
        let mut refused = false;
        for _ in 0..100 {
            match poll(&mut fresh.app) {
                Err(Error::Protocol) => {
                    refused = true;
                    break;
                }
                Ok(Progress::Frame(_)) => panic!("unconfirmed data was delivered"),
                Ok(_) => {}
                Err(error) => panic!("unexpected failure: {error}"),
            }
        }
        assert!(refused);
        assert!(!fresh.wire.lock().unwrap().open[1]);
    }
}

#[test]
fn partial_ciphertext_is_discarded_on_revocation_without_epoch_replay() {
    let mut pair = pair(1, false, MAX_BOOTSTRAP_DURATION);
    let (adapter_display, app_display) = displays(&mut pair);
    approve_both(&pair, &adapter_display, &app_display);
    establish(&mut pair);
    let before = pair.wire.lock().unwrap().written[1].len();
    pair.app
        .prepare_send(
            b"revoked terminal content",
            Instant::now() + MAX_FRAME_DURATION,
        )
        .unwrap();
    for _ in 0..20 {
        let _ = pair.app.poll(|| Ok(())).unwrap();
        if pair.wire.lock().unwrap().written[1].len() > before {
            break;
        }
    }
    let partial = pair.wire.lock().unwrap().written[1].len();
    assert_eq!(partial, before + 1);
    assert_eq!(
        pair.app
            .poll::<()>(|| Err(io::ErrorKind::PermissionDenied.into()))
            .unwrap_err(),
        Error::Denied
    );
    assert_eq!(pair.wire.lock().unwrap().written[1].len(), partial);
    assert!(!pair.wire.lock().unwrap().open[1]);
    assert_eq!(
        pair.app
            .prepare_send(b"new epoch", Instant::now() + MAX_FRAME_DURATION),
        Err(Error::Closed)
    );
    for _ in 0..10 {
        match poll(&mut pair.adapter) {
            Ok(Progress::Frame(_)) => panic!("partial ciphertext must not release a frame"),
            Err(Error::PeerClosed) => return,
            Ok(_) => {}
            Err(error) => panic!("unexpected failure: {error}"),
        }
    }
    panic!("peer did not observe the closed partial frame");
}

#[test]
fn absent_confirmation_and_backpressured_frame_have_bounded_deadlines() {
    let mut absent = pair(usize::MAX, false, Duration::from_millis(150));
    let (_, app_display) = displays(&mut absent);
    absent
        .adapter_decision
        .send(compared(&app_display))
        .unwrap();
    for _ in 0..10 {
        let _ = poll(&mut absent.adapter).unwrap();
    }
    thread::sleep(Duration::from_millis(180));
    assert_eq!(poll(&mut absent.adapter).unwrap_err(), Error::Deadline);
    assert!(!absent.wire.lock().unwrap().open[0]);

    let mut pair = pair(usize::MAX, false, MAX_BOOTSTRAP_DURATION);
    let (adapter_display, app_display) = displays(&mut pair);
    approve_both(&pair, &adapter_display, &app_display);
    establish(&mut pair);
    pair.adapter_blocked.store(true, Ordering::Release);
    pair.adapter
        .prepare_send(b"blocked", Instant::now() + Duration::from_millis(30))
        .unwrap();
    assert!(matches!(
        poll(&mut pair.adapter).unwrap(),
        Progress::Blocked
    ));
    thread::sleep(Duration::from_millis(50));
    assert_eq!(poll(&mut pair.adapter).unwrap_err(), Error::Deadline);
    assert!(!pair.wire.lock().unwrap().open[0]);
}

#[test]
fn eof_while_awaiting_native_decision_discards_unverified_state() {
    let mut pair = pair(usize::MAX, false, MAX_BOOTSTRAP_DURATION);
    let _ = displays(&mut pair);
    pair.app.close().unwrap();
    assert!(pair.app_cancelled.load(Ordering::Acquire));
    assert_eq!(poll(&mut pair.adapter).unwrap_err(), Error::PeerClosed);
    assert!(!pair.wire.lock().unwrap().open[0]);
    assert!(pair.adapter_cancelled.load(Ordering::Acquire));
}

#[test]
fn interrupted_admission_discards_a_partially_written_frame() {
    let mut pair = pair(1, false, MAX_BOOTSTRAP_DURATION);
    let (adapter_display, app_display) = displays(&mut pair);
    approve_both(&pair, &adapter_display, &app_display);
    establish(&mut pair);
    let before = pair.wire.lock().unwrap().written[1].len();
    pair.app
        .prepare_send(b"revoked", Instant::now() + MAX_FRAME_DURATION)
        .unwrap();
    for _ in 0..10 {
        let _ = poll(&mut pair.app).unwrap();
        if pair.wire.lock().unwrap().written[1].len() > before {
            break;
        }
    }
    assert_eq!(pair.wire.lock().unwrap().written[1].len(), before + 1);
    assert_eq!(
        pair.app
            .poll::<()>(|| Err(io::ErrorKind::Interrupted.into()))
            .unwrap_err(),
        Error::Denied
    );
    assert!(!pair.wire.lock().unwrap().open[1]);
    assert_eq!(pair.wire.lock().unwrap().written[1].len(), before + 1);
    assert_eq!(poll(&mut pair.app).unwrap_err(), Error::Closed);
}

#[test]
fn physical_timeout_and_late_completion_cannot_resume_a_frame() {
    let mut pair = pair_with_frame_duration(
        usize::MAX,
        false,
        MAX_BOOTSTRAP_DURATION,
        Duration::from_millis(100),
    );
    let (adapter_display, app_display) = displays(&mut pair);
    approve_both(&pair, &adapter_display, &app_display);
    establish(&mut pair);
    pair.wire.lock().unwrap().timeout_write[0] = true;
    pair.adapter
        .prepare_send(b"timeout", Instant::now() + MAX_FRAME_DURATION)
        .unwrap();
    let mut refused = false;
    for _ in 0..10 {
        match poll(&mut pair.adapter) {
            Err(Error::Protocol) => {
                refused = true;
                break;
            }
            Ok(_) => {}
            Err(error) => panic!("unexpected failure: {error}"),
        }
    }
    assert!(refused);
    assert!(!pair.wire.lock().unwrap().open[0]);

    let mut late = pair_with_frame_duration(
        usize::MAX,
        false,
        MAX_BOOTSTRAP_DURATION,
        Duration::from_millis(100),
    );
    let (adapter_display, app_display) = displays(&mut late);
    approve_both(&late, &adapter_display, &app_display);
    establish(&mut late);
    late.wire.lock().unwrap().write_delay[1] = Duration::from_millis(130);
    late.app
        .prepare_send(b"late", Instant::now() + MAX_FRAME_DURATION)
        .unwrap();
    let mut expired = false;
    for _ in 0..10 {
        match poll(&mut late.app) {
            Err(Error::Deadline) => {
                expired = true;
                break;
            }
            Ok(Progress::Sent) => panic!("a late physical write must not report success"),
            Ok(_) => {}
            Err(error) => panic!("unexpected failure: {error}"),
        }
    }
    assert!(expired);
    assert!(!late.wire.lock().unwrap().open[1]);
}

#[test]
fn prefetched_following_frame_keeps_its_original_read_deadline() {
    let mut pair = pair_with_frame_duration(
        usize::MAX,
        false,
        MAX_BOOTSTRAP_DURATION,
        Duration::from_millis(100),
    );
    let (adapter_display, app_display) = displays(&mut pair);
    approve_both(&pair, &adapter_display, &app_display);
    establish(&mut pair);
    for payload in [b"first".as_slice(), b"second".as_slice()] {
        pair.adapter
            .prepare_send(payload, Instant::now() + MAX_FRAME_DURATION)
            .unwrap();
        for _ in 0..10 {
            if matches!(poll(&mut pair.adapter).unwrap(), Progress::Sent) {
                break;
            }
        }
        assert!(!pair.adapter.has_pending_output());
    }
    let mut first = None;
    for _ in 0..10 {
        if let Progress::Frame(frame) = poll(&mut pair.app).unwrap() {
            first = Some(frame);
            break;
        }
    }
    assert_eq!(first.as_deref(), Some(b"first".as_slice()));
    thread::sleep(Duration::from_millis(130));
    assert_eq!(poll(&mut pair.app).unwrap_err(), Error::Deadline);
}

#[test]
fn panicking_admission_callback_destructor_closes_the_stream() {
    struct PanicOnDrop;
    impl Drop for PanicOnDrop {
        fn drop(&mut self) {
            panic!("test callback destructor");
        }
    }
    let mut first = pair(usize::MAX, false, MAX_BOOTSTRAP_DURATION);
    let on_drop = PanicOnDrop;
    assert_eq!(
        first
            .adapter
            .poll(move || {
                let _ = &on_drop;
                Ok(())
            })
            .unwrap_err(),
        Error::CallbackPanicked
    );
    assert!(!first.wire.lock().unwrap().open[0]);
    assert_eq!(poll(&mut first.adapter).unwrap_err(), Error::Closed);

    let mut second = pair(usize::MAX, false, MAX_BOOTSTRAP_DURATION);
    let on_drop = PanicOnDrop;
    assert_eq!(
        second
            .adapter
            .poll(move || -> io::Result<()> {
                let _ = &on_drop;
                panic!("test callback invocation");
            })
            .unwrap_err(),
        Error::CallbackPanicked
    );
    assert!(!second.wire.lock().unwrap().open[0]);
}

#[test]
fn writer_and_admission_guard_panics_still_close_the_stream() {
    struct PanicGuard;
    impl Drop for PanicGuard {
        fn drop(&mut self) {
            panic!("test admission guard");
        }
    }
    let mut pair = pair(usize::MAX, false, MAX_BOOTSTRAP_DURATION);
    pair.wire.lock().unwrap().panic_write[0] = true;
    assert_eq!(
        pair.adapter.poll(|| Ok(PanicGuard)).unwrap_err(),
        Error::CallbackPanicked
    );
    assert!(!pair.wire.lock().unwrap().open[0]);
}

#[test]
fn panicking_native_callback_cancels_the_prompt_and_discards_the_stream() {
    let mut pair = pair(usize::MAX, false, MAX_BOOTSTRAP_DURATION);
    let _ = displays(&mut pair);
    pair.adapter_panic.store(true, Ordering::Release);
    assert_eq!(
        poll(&mut pair.adapter).unwrap_err(),
        Error::CallbackPanicked
    );
    assert!(pair.adapter_cancelled.load(Ordering::Acquire));
    assert!(!pair.wire.lock().unwrap().open[0]);
    assert_eq!(poll(&mut pair.adapter).unwrap_err(), Error::Closed);
}

#[test]
fn native_cancel_and_destructor_panics_still_close_the_owned_stream() {
    struct PanickingNative {
        cancelled: Arc<AtomicBool>,
        dropped: Arc<AtomicBool>,
        panic_cancel: bool,
        panic_drop: bool,
    }

    impl TrustedNativeVerification for PanickingNative {
        fn show_local_fingerprint(&mut self, _: &str) -> Result<(), NativeUnavailable> {
            Ok(())
        }

        fn poll_decision(&mut self) -> NativeDecision {
            NativeDecision::Pending
        }

        fn cancel(&mut self) -> Result<(), NativeCleanupFailed> {
            self.cancelled.store(true, Ordering::Release);
            assert!(!self.panic_cancel, "test cancellation");
            Ok(())
        }
    }

    impl Drop for PanickingNative {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
            assert!(!self.panic_drop, "test verifier destructor");
        }
    }

    for (during_unwind, panic_cancel, panic_drop) in [
        (false, true, false),
        (false, false, true),
        (false, true, true),
        (true, true, true),
    ] {
        let wire = Arc::new(Mutex::new(Wire {
            open: [true, true],
            ..Wire::default()
        }));
        let cancelled = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicBool::new(false));
        let mut transport = SecureTransport::new(
            TestStream {
                side: 0,
                wire: wire.clone(),
                blocked: Arc::new(AtomicBool::new(false)),
                chunk: 1,
                interruptions: false,
                reads: 0,
                writes: 0,
            },
            Role::Application,
            PanickingNative {
                cancelled: cancelled.clone(),
                dropped: dropped.clone(),
                panic_cancel,
                panic_drop,
            },
            MAX_BOOTSTRAP_DURATION,
            MAX_FRAME_DURATION,
        )
        .unwrap();
        let status = transport.cleanup_status();
        if during_unwind {
            let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _transport = transport;
                panic!("test outer unwind");
            }));
            assert!(unwind.is_err());
        } else {
            assert_eq!(transport.close(), Err(Error::CleanupFailed));
            assert_eq!(transport.close(), Err(Error::CleanupFailed));
            assert_eq!(transport.poll(|| Ok(())).unwrap_err(), Error::CleanupFailed);
            drop(transport);
        }
        assert!(status.failed());
        assert!(cancelled.load(Ordering::Acquire));
        assert!(dropped.load(Ordering::Acquire));
        assert!(!wire.lock().unwrap().open[0]);
    }
}

#[test]
fn native_retirement_precedes_stream_disposal_and_failure_is_sticky() {
    struct Stream(Arc<Mutex<Vec<&'static str>>>);
    impl Read for Stream {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::WouldBlock.into())
        }
    }
    impl Write for Stream {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::WouldBlock.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl Drop for Stream {
        fn drop(&mut self) {
            self.0.lock().unwrap().push("stream");
        }
    }

    struct Verifier {
        order: Arc<Mutex<Vec<&'static str>>>,
        failure: u8,
    }
    impl TrustedNativeVerification for Verifier {
        fn show_local_fingerprint(&mut self, _: &str) -> Result<(), NativeUnavailable> {
            Err(NativeUnavailable)
        }
        fn poll_decision(&mut self) -> NativeDecision {
            NativeDecision::Denied
        }
        fn begin_retirement(&mut self) -> Result<(), NativeCleanupFailed> {
            self.order.lock().unwrap().push("retire");
            match self.failure {
                0 => Ok(()),
                1 => Err(NativeCleanupFailed),
                _ => panic!("synthetic retirement panic"),
            }
        }
        fn cancel(&mut self) -> Result<(), NativeCleanupFailed> {
            self.order.lock().unwrap().push("cancel");
            Ok(())
        }
    }
    impl Drop for Verifier {
        fn drop(&mut self) {
            self.order.lock().unwrap().push("native");
        }
    }

    for invalid_constructor in [false, true] {
        for failure in 0..=2 {
            let order = Arc::new(Mutex::new(Vec::new()));
            let result = SecureTransport::new(
                Stream(order.clone()),
                Role::Application,
                Verifier {
                    order: order.clone(),
                    failure,
                },
                if invalid_constructor {
                    Duration::ZERO
                } else {
                    MAX_BOOTSTRAP_DURATION
                },
                MAX_FRAME_DURATION,
            );
            if invalid_constructor {
                match result {
                    Err(error) => assert_eq!(
                        error,
                        if failure == 0 {
                            Error::Protocol
                        } else {
                            Error::CleanupFailed
                        }
                    ),
                    Ok(_) => panic!("invalid constructor was accepted"),
                }
            } else {
                let mut transport = result.unwrap();
                let status = transport.cleanup_status();
                let expected = if failure == 0 {
                    Ok(())
                } else {
                    Err(Error::CleanupFailed)
                };
                assert_eq!(transport.close(), expected);
                assert_eq!(transport.close(), expected);
                assert_eq!(status.failed(), failure != 0);
                drop(transport);
            }
            assert_eq!(
                *order.lock().unwrap(),
                ["retire", "stream", "cancel", "native"]
            );
        }
    }
}

#[test]
fn caught_cleanup_panic_still_reaches_process_panic_hook() {
    const CHILD: &str = "FESTERM_IPC_CLEANUP_HOOK_TEST_CHILD";
    const PAYLOAD: &str = "synthetic cleanup panic";
    static OBSERVED: AtomicBool = AtomicBool::new(false);

    let args: Vec<_> = std::env::args_os().collect();
    let isolated_child = std::env::var_os(CHILD).is_some()
        && args.iter().any(|arg| arg == "--exact")
        && args
            .iter()
            .any(|arg| arg == "caught_cleanup_panic_still_reaches_process_panic_hook")
        && args.iter().any(|arg| arg == "--test-threads=1");
    if isolated_child {
        std::panic::set_hook(Box::new(|information| {
            if information.payload().downcast_ref::<&str>() == Some(&PAYLOAD) {
                OBSERVED.store(true, Ordering::Release);
            }
        }));
        struct PanickingNative;
        impl TrustedNativeVerification for PanickingNative {
            fn show_local_fingerprint(&mut self, _: &str) -> Result<(), NativeUnavailable> {
                Ok(())
            }
            fn poll_decision(&mut self) -> NativeDecision {
                NativeDecision::Pending
            }
            fn cancel(&mut self) -> Result<(), NativeCleanupFailed> {
                panic!("synthetic cleanup panic");
            }
        }
        let wire = Arc::new(Mutex::new(Wire {
            open: [true, true],
            ..Wire::default()
        }));
        let mut transport = SecureTransport::new(
            TestStream {
                side: 0,
                wire: wire.clone(),
                blocked: Arc::new(AtomicBool::new(false)),
                chunk: 1,
                interruptions: false,
                reads: 0,
                writes: 0,
            },
            Role::Application,
            PanickingNative,
            MAX_BOOTSTRAP_DURATION,
            MAX_FRAME_DURATION,
        )
        .unwrap();
        let status = transport.cleanup_status();
        assert_eq!(transport.close(), Err(Error::CleanupFailed));
        assert!(status.failed());
        assert_eq!(
            Error::CleanupFailed.to_string(),
            "secure transport cleanup failed"
        );
        assert!(!wire.lock().unwrap().open[0]);
        assert!(OBSERVED.load(Ordering::Acquire));
        return;
    }

    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "caught_cleanup_panic_still_reaches_process_panic_hook",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(child.status.success(), "isolated hook probe failed");
}

#[test]
fn expired_partial_input_prevents_queuing_a_response() {
    let mut pair = pair_with_frame_duration(
        usize::MAX,
        false,
        MAX_BOOTSTRAP_DURATION,
        Duration::from_millis(80),
    );
    let (adapter, app) = displays(&mut pair);
    approve_both(&pair, &adapter, &app);
    establish(&mut pair);
    pair.wire.lock().unwrap().incoming[1].push_back(0);
    for _ in 0..10 {
        let _ = poll(&mut pair.app).unwrap();
        if pair.wire.lock().unwrap().incoming[1].is_empty() {
            break;
        }
    }
    assert!(pair.wire.lock().unwrap().incoming[1].is_empty());
    thread::sleep(Duration::from_millis(100));
    assert_eq!(
        pair.app
            .prepare_send(b"response", Instant::now() + MAX_FRAME_DURATION),
        Err(Error::Deadline)
    );
    assert!(!pair.wire.lock().unwrap().open[1]);
}

#[test]
fn active_relay_can_delay_ciphertext_after_the_real_adapter_closes() {
    let mut pair = pair(usize::MAX, false, MAX_BOOTSTRAP_DURATION);
    let (adapter_display, app_display) = displays(&mut pair);
    approve_both(&pair, &adapter_display, &app_display);
    establish(&mut pair);
    pair.adapter
        .prepare_send(b"delayed request", Instant::now() + MAX_FRAME_DURATION)
        .unwrap();
    for _ in 0..100 {
        if matches!(poll(&mut pair.adapter).unwrap(), Progress::Sent) {
            break;
        }
    }
    let withheld: Vec<_> = pair.wire.lock().unwrap().incoming[1].drain(..).collect();
    assert!(!withheld.is_empty());
    pair.adapter.close().unwrap();
    {
        let mut wire = pair.wire.lock().unwrap();
        wire.open[0] = true;
    }
    for _ in 0..5 {
        assert!(matches!(poll(&mut pair.app).unwrap(), Progress::Blocked));
    }
    pair.wire.lock().unwrap().incoming[1].extend(withheld);
    for _ in 0..100 {
        if let Progress::Frame(frame) = poll(&mut pair.app).unwrap() {
            assert_eq!(frame, b"delayed request");
            return;
        }
    }
    panic!("a live relay should be able to deliver previously valid ciphertext");
}
