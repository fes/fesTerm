//! Dormant direct-channel bridge. This does not open a listener or qualify a
//! native caller, privacy monitor, recipient, or effect boundary.

mod owner;
#[cfg(all(test, unix))]
mod process_tests;
#[cfg(all(test, windows))]
mod windows_tests;

use super::super::verification::{OwnerLiveness, Verifier};
use super::{Connection, Event, Waker};
use festerm_automation::{outbound_channel, OutboundFrame, OutboundReceiver};
use festerm_automation_mcp::secure_transport::{
    self, IoInterest, Progress, SecureTransport, TrustedNativeVerification,
};
use std::{
    future::Future,
    io::{self, Read, Write},
    mem,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{SyncSender, TryRecvError},
        Arc,
    },
    time::Instant,
};
use tokio::sync::Notify;

/// Wake the owner after a native decision, admission change, or lifecycle cut.
#[derive(Clone, Default)]
pub(super) struct Wake(Arc<Notify>);

impl Wake {
    pub(super) fn notify(&self) {
        self.0.notify_one();
    }

    pub(super) fn notified(&self) -> impl Future<Output = ()> + '_ {
        self.0.notified()
    }
}

/// A separate native caller/recipient fence. No implementation is qualified.
///
/// The guard must be nonblocking and remain valid through a physical write or
/// incoming event insertion. Temporary write contention returns `WouldBlock`
/// and wakes the owner when it changes. Incoming contention closes rather than
/// hiding an unqueued cancellation from the coordinator. The eventual owner
/// must also couple caller loss to connection retirement and every effect
/// boundary; this trait alone does not establish that coupling.
///
/// The native guard must not acquire the non-reentrant application admission
/// lock. The bridge acquires that lock after this guard and releases it first.
/// Callbacks and destructors must not block or panic with sensitive messages.
/// A process panic hook runs before a panic can be caught.
pub(super) trait NativeAdmission {
    type Guard<'a>
    where
        Self: 'a;

    fn try_admit(&self) -> io::Result<Self::Guard<'_>>;
    fn cancel(&mut self) -> io::Result<()>;
}

/// The verifier's wake must notify the same owner when its UI owner is lost.
pub(super) trait HostVerification: TrustedNativeVerification {
    fn owner_liveness(&self) -> OwnerLiveness;
}

impl HostVerification for Verifier {
    fn owner_liveness(&self) -> OwnerLiveness {
        Verifier::owner_liveness(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Error {
    Channel(secure_transport::Error),
    Unqualified,
    Admission,
    Events,
    Cleanup,
    Closed,
    OwnerInterrupted,
    OwnerLost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Step {
    Advanced,
    Blocked,
    Closed,
}

/// An opaque production gate. It cannot be obtained before native qualification.
pub(super) struct QualifiedBootstrap(());

impl QualifiedBootstrap {
    pub(super) fn require() -> Result<Self, Error> {
        Err(Error::Unqualified)
    }

    #[cfg(test)]
    fn synthetic() -> Self {
        Self(())
    }
}

pub(super) struct PeerContext {
    pub(super) bootstrap: QualifiedBootstrap,
    pub(super) key: u64,
    pub(super) events: SyncSender<Event>,
    pub(super) wake_ui: Waker,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) privacy_closed: Arc<AtomicBool>,
    pub(super) wake: Wake,
}

struct HeldFrame(OutboundFrame);

impl Drop for HeldFrame {
    fn drop(&mut self) {
        self.0.bytes.fill(0);
    }
}

/// One exclusively owned channel, with the original host frame retained until
/// every encrypted byte has been physically written or the channel is closed.
///
/// The I/O owner calls `step` immediately after progress and waits on socket
/// readiness or pipe retry, `wake`, or `next_deadline` after `Blocked`. It must notify `wake`
/// after native decisions, admission changes and lifecycle cuts that do not
/// close an announced connection. Guarded connection closes defer their wake
/// until admission is released. Run this off the UI thread: the existing host
/// admission boundary may briefly block.
pub(super) struct Peer<S: Read + Write, N: TrustedNativeVerification, A: NativeAdmission> {
    channel: SecureTransport<S, N>,
    admission: Option<A>,
    connection: Connection,
    output: OutboundReceiver,
    events: SyncSender<Event>,
    wake_ui: Waker,
    stop: Arc<AtomicBool>,
    privacy_closed: Arc<AtomicBool>,
    owner_liveness: OwnerLiveness,
    pending: Option<HeldFrame>,
    announced: bool,
    admission_blocked: bool,
    finished: bool,
    cleanup_failed: bool,
}

impl<S: Read + Write, N: TrustedNativeVerification, A: NativeAdmission> Peer<S, N, A> {
    fn new(
        channel: SecureTransport<S, N>,
        admission: A,
        context: PeerContext,
        owner_liveness: OwnerLiveness,
    ) -> Self {
        let (output, receiver) = outbound_channel();
        let PeerContext {
            bootstrap: _,
            key,
            events,
            wake_ui,
            stop,
            privacy_closed,
            wake,
        } = context;
        Self {
            channel,
            admission: Some(admission),
            connection: Connection {
                key,
                closed: Arc::new(AtomicBool::new(false)),
                pending_input: Arc::new(AtomicUsize::new(0)),
                output,
                output_wake: Some(wake),
            },
            output: receiver,
            events,
            wake_ui,
            stop,
            privacy_closed,
            owner_liveness,
            pending: None,
            announced: false,
            admission_blocked: false,
            finished: false,
            cleanup_failed: false,
        }
    }

    pub(super) fn io_interest(&self) -> IoInterest {
        let mut interest = self.channel.io_interest();
        if self.admission_blocked {
            interest.writable = false;
        }
        interest
    }

    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.channel.next_deadline()
    }

    pub(super) fn step(&mut self) -> Result<Step, Error> {
        if self.finished {
            return Err(Error::Closed);
        }
        let result = self.step_inner();
        match result {
            Ok(Step::Closed) => {
                self.finish()?;
                Ok(Step::Closed)
            }
            Err(error) => {
                self.finish()?;
                Err(error)
            }
            other => other,
        }
    }

    fn step_inner(&mut self) -> Result<Step, Error> {
        // This wake-driven cut does not fence an effect already in flight.
        if self.connection.is_closed()
            || self.stop.load(Ordering::Acquire)
            || self.privacy_closed.load(Ordering::Acquire)
            || !self.owner_liveness.is_alive()
        {
            return Ok(Step::Closed);
        }

        if self.channel.is_established() && !self.announced {
            let admission = self.admission.as_ref().ok_or(Error::Closed)?;
            let native = admission.try_admit().map_err(|_| Error::Admission)?;
            let guard = festerm_session::automation_admission_lock().ok_or(Error::Admission)?;
            if self.connection.is_closed()
                || self.stop.load(Ordering::Acquire)
                || !self.owner_liveness.is_alive()
                || !super::super::try_privacy_admission(&self.privacy_closed)
            {
                return Ok(Step::Closed);
            }
            self.events
                .try_send(Event::Connected {
                    connection: self.connection.clone(),
                    identity: None,
                })
                .map_err(|_| Error::Events)?;
            drop(guard);
            drop(native);
            self.announced = true;
            (self.wake_ui)();
            return Ok(Step::Advanced);
        }

        if self.announced && self.pending.is_none() {
            match self.output.try_recv() {
                Ok(frame) => {
                    let frame = HeldFrame(frame);
                    self.channel
                        .prepare_send(&frame.0.bytes, frame.0.expires)
                        .map_err(Error::Channel)?;
                    self.pending = Some(frame);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return Ok(Step::Closed),
            }
        }

        let admission = self.admission.as_ref().ok_or(Error::Closed)?;
        let pending = self.pending.as_ref();
        let connection = &self.connection;
        let stop = &self.stop;
        let privacy_closed = &self.privacy_closed;
        let owner_liveness = &self.owner_liveness;
        let mut admission_blocked = false;
        let progress = self.channel.poll(|| {
            let native = match admission.try_admit() {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    admission_blocked = true;
                    return Err(error);
                }
                result => result?,
            };
            let guard = festerm_session::automation_admission_lock()
                .ok_or_else(|| io::Error::other("automation admission unavailable"))?;
            if connection.is_closed()
                || stop.load(Ordering::Acquire)
                || !owner_liveness.is_alive()
                || pending
                    .is_some_and(|frame| Instant::now() >= frame.0.expires || !frame.0.is_valid())
                || !super::super::try_privacy_admission(privacy_closed)
            {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok((guard, native))
        });
        self.admission_blocked = admission_blocked;
        match progress.map_err(Error::Channel)? {
            Progress::Blocked => Ok(Step::Blocked),
            Progress::Advanced | Progress::Established => Ok(Step::Advanced),
            Progress::Sent => {
                self.pending.take().ok_or(Error::Closed)?;
                Ok(Step::Advanced)
            }
            Progress::Frame(mut bytes) => {
                if !self.owner_liveness.is_alive() {
                    bytes.fill(0);
                    return Ok(Step::Closed);
                }
                let native = admission.try_admit().map_err(|_| Error::Admission)?;
                self.connection
                    .enqueue_message(&self.events, bytes, &self.stop, &self.privacy_closed)
                    .map_err(|_| Error::Events)?;
                drop(native);
                (self.wake_ui)();
                Ok(Step::Advanced)
            }
        }
    }

    pub(super) fn finish(&mut self) -> Result<(), Error> {
        if self.finished {
            return if self.cleanup_failed {
                Err(Error::Cleanup)
            } else {
                Ok(())
            };
        }
        self.finished = true;
        self.connection.close();
        self.pending.take();
        while let Ok(mut frame) = self.output.try_recv() {
            frame.bytes.fill(0);
        }
        let channel_ok = self.channel.close().is_ok();
        let admission_ok = self.admission.take().is_none_or(retire_admission);
        self.cleanup_failed = !channel_ok || !admission_ok;
        (self.wake_ui)();
        if self.cleanup_failed {
            Err(Error::Cleanup)
        } else {
            Ok(())
        }
    }
}

impl<S: Read + Write, N: TrustedNativeVerification, A: NativeAdmission> Drop for Peer<S, N, A> {
    fn drop(&mut self) {
        if self.finish().is_err() {
            tracing::warn!("direct automation peer cleanup failed");
        }
    }
}

fn cleanup_step(operation: impl FnOnce() -> io::Result<()>) -> bool {
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(Ok(())) => true,
        Ok(Err(_)) => false,
        Err(payload) => {
            if let Err(payload) = catch_unwind(AssertUnwindSafe(|| drop(payload))) {
                mem::forget(payload);
            }
            false
        }
    }
}

fn retire_admission<A: NativeAdmission>(mut admission: A) -> bool {
    let cancelled = cleanup_step(|| admission.cancel());
    let dropped = cleanup_step(|| {
        drop(admission);
        Ok(())
    });
    cancelled && dropped
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::super::{verification, PrivacyGate};
    use super::*;
    use festerm_automation_mcp::secure_transport::{
        NativeCleanupFailed, NativeDecision, NativeUnavailable, Role, MAX_FRAME_DURATION,
    };
    use std::{
        os::unix::net::UnixStream,
        sync::{mpsc, Mutex},
        thread,
        time::Duration,
    };

    #[derive(Default)]
    pub(super) struct NativeState {
        pub(super) displayed: Mutex<Option<String>>,
        pub(super) decision: Mutex<Option<[u8; 64]>>,
        pub(super) cancelled: AtomicBool,
        owner_liveness: OwnerLiveness,
    }

    pub(super) struct Native(pub(super) Arc<NativeState>);

    impl TrustedNativeVerification for Native {
        fn show_local_fingerprint(&mut self, fingerprint: &str) -> Result<(), NativeUnavailable> {
            *self.0.displayed.lock().unwrap() = Some(fingerprint.to_owned());
            Ok(())
        }

        fn poll_decision(&mut self) -> NativeDecision {
            match self.0.decision.lock().unwrap().take() {
                Some(fingerprint) => NativeDecision::ComparedPeer(fingerprint),
                None => NativeDecision::Pending,
            }
        }

        fn cancel(&mut self) -> Result<(), NativeCleanupFailed> {
            self.0.cancelled.store(true, Ordering::Release);
            Ok(())
        }
    }

    impl HostVerification for Native {
        fn owner_liveness(&self) -> OwnerLiveness {
            self.0.owner_liveness.clone()
        }
    }

    fn reserve_verifier(
        owner: &verification::Owner,
        stop: Arc<AtomicBool>,
        privacy: Arc<PrivacyGate>,
        wake: &Wake,
    ) -> Verifier {
        let surface = owner.surface();
        let wake = wake.clone();
        let wake_owner = Arc::new(move || wake.notify());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match surface.reserve(
                17,
                crate::application::WindowId::PRIMARY,
                stop.clone(),
                privacy.clone(),
                Arc::new(|| {}),
                wake_owner.clone(),
            ) {
                Ok(verifier) => return verifier,
                Err(verification::Error::Busy) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("could not reserve comparison: {error:?}"),
            }
        }
    }

    pub(super) struct Admission {
        pub(super) open: Arc<AtomicBool>,
        pub(super) cancelled: Arc<AtomicBool>,
        pub(super) fail_cleanup: bool,
    }

    impl NativeAdmission for Admission {
        type Guard<'a> = ();

        fn try_admit(&self) -> io::Result<Self::Guard<'_>> {
            if self.open.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err(io::ErrorKind::WouldBlock.into())
            }
        }

        fn cancel(&mut self) -> io::Result<()> {
            self.cancelled.store(true, Ordering::Release);
            if self.fail_cleanup {
                Err(io::Error::other("test cleanup failure"))
            } else {
                Ok(())
            }
        }
    }

    struct Fixture<S: Read + Write> {
        host: Peer<S, Native, Admission>,
        client: SecureTransport<UnixStream, Native>,
        events: mpsc::Receiver<Event>,
        host_native: Arc<NativeState>,
        client_native: Arc<NativeState>,
        open: Arc<AtomicBool>,
        cancelled: Arc<AtomicBool>,
        wake: Wake,
    }

    #[test]
    fn direct_host_bootstrap_refuses_without_native_qualification() {
        assert!(matches!(
            QualifiedBootstrap::require(),
            Err(Error::Unqualified)
        ));
    }

    impl<S: Read + Write> Fixture<S> {
        fn new(wrap: impl FnOnce(UnixStream) -> S) -> Self {
            let (host_socket, client_socket) = UnixStream::pair().unwrap();
            host_socket.set_nonblocking(true).unwrap();
            client_socket.set_nonblocking(true).unwrap();
            let host_native = Arc::new(NativeState::default());
            let client_native = Arc::new(NativeState::default());
            let host = SecureTransport::new(
                wrap(host_socket),
                Role::Application,
                Native(host_native.clone()),
                Duration::from_secs(8),
                MAX_FRAME_DURATION,
            )
            .unwrap();
            let client = SecureTransport::new(
                client_socket,
                Role::Client,
                Native(client_native.clone()),
                Duration::from_secs(8),
                MAX_FRAME_DURATION,
            )
            .unwrap();
            let open = Arc::new(AtomicBool::new(true));
            let cancelled = Arc::new(AtomicBool::new(false));
            let wake = Wake::default();
            let (sender, events) = mpsc::sync_channel(8);
            Self {
                host: Peer::new(
                    host,
                    Admission {
                        open: open.clone(),
                        cancelled: cancelled.clone(),
                        fail_cleanup: false,
                    },
                    PeerContext {
                        bootstrap: QualifiedBootstrap::synthetic(),
                        key: 17,
                        events: sender,
                        wake_ui: Arc::new(|| {}),
                        stop: Arc::new(AtomicBool::new(false)),
                        privacy_closed: Arc::new(AtomicBool::new(false)),
                        wake: wake.clone(),
                    },
                    host_native.owner_liveness.clone(),
                ),
                client,
                events,
                host_native,
                client_native,
                open,
                cancelled,
                wake,
            }
        }

        fn pump(&mut self) {
            assert_ne!(self.host.step().unwrap(), Step::Closed);
            assert!(!matches!(
                self.client.poll(|| Ok(())).unwrap(),
                Progress::Frame(_)
            ));
        }

        fn establish(&mut self) -> Connection {
            let deadline = Instant::now() + Duration::from_secs(5);
            let (host_code, client_code) = loop {
                self.pump();
                let host = self.host_native.displayed.lock().unwrap().clone();
                let client = self.client_native.displayed.lock().unwrap().clone();
                if let (Some(host), Some(client)) = (host, client) {
                    break (host, client);
                }
                assert!(Instant::now() < deadline, "fingerprint exchange timed out");
                thread::sleep(Duration::from_millis(1));
            };
            assert_eq!(host_code, client_code);
            *self.client_native.decision.lock().unwrap() =
                Some(host_code.as_bytes().try_into().unwrap());
            let withheld_until = Instant::now() + Duration::from_millis(30);
            while Instant::now() < withheld_until {
                self.pump();
                assert!(self.events.try_recv().is_err());
                thread::sleep(Duration::from_millis(1));
            }
            *self.host_native.decision.lock().unwrap() =
                Some(client_code.as_bytes().try_into().unwrap());
            let mut connection = None;
            while connection.is_none() || !self.client.is_established() {
                self.pump();
                if let Ok(event) = self.events.try_recv() {
                    match event {
                        Event::Connected { connection: peer, .. } => connection = Some(peer),
                        Event::Message { .. } => panic!("message preceded connection"),
                    }
                }
                assert!(Instant::now() < deadline, "confirmation timed out");
                thread::sleep(Duration::from_millis(1));
            }
            connection.unwrap()
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn direct_peer_bridges_guarded_host_events_and_wakes_output() {
        let mut fixture = Fixture::new(|socket| socket);
        let connection = fixture.establish();
        fixture
            .client
            .prepare_send(b"request", Instant::now() + MAX_FRAME_DURATION)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            fixture.pump();
            if let Ok(Event::Message { key, bytes }) = fixture.events.try_recv() {
                assert_eq!(key, 17);
                assert_eq!(bytes, b"request");
                break;
            }
            assert!(Instant::now() < deadline, "request was not enqueued");
            thread::sleep(Duration::from_millis(1));
        }
        let guard = festerm_session::automation_admission_lock().unwrap();
        assert!(connection.has_pending_input(&guard));
        drop(guard);
        connection.finish_message().unwrap();
        connection
            .send(
                b"response".to_vec(),
                Instant::now() + MAX_FRAME_DURATION,
                &[],
            )
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), fixture.wake.notified())
            .await
            .expect("queued response did not wake its owner");
        loop {
            assert_ne!(fixture.host.step().unwrap(), Step::Closed);
            if let Progress::Frame(bytes) = fixture.client.poll(|| Ok(())).unwrap() {
                assert_eq!(bytes, b"response");
                break;
            }
            assert!(Instant::now() < deadline, "response was not delivered");
            thread::sleep(Duration::from_millis(1));
        }
        fixture.host.finish().unwrap();
        assert!(connection.is_closed());
        assert!(fixture.host_native.cancelled.load(Ordering::Acquire));
        assert!(fixture.cancelled.load(Ordering::Acquire));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn direct_peer_queue_overflow_closes_and_wakes_owner() {
        let mut fixture = Fixture::new(|socket| socket);
        let connection = fixture.host.connection.clone();
        for _ in 0..4 {
            connection
                .send(b"queued".to_vec(), Instant::now() + MAX_FRAME_DURATION, &[])
                .unwrap();
        }
        fixture.wake.notified().await;
        assert_eq!(
            connection
                .send(
                    b"overflow".to_vec(),
                    Instant::now() + MAX_FRAME_DURATION,
                    &[]
                )
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        tokio::time::timeout(Duration::from_secs(1), fixture.wake.notified())
            .await
            .expect("overflow did not wake its owner");
        assert!(connection.is_closed());
        fixture.host.finish().unwrap();
        assert!(matches!(
            fixture.host.output.try_recv(),
            Err(TryRecvError::Empty)
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn direct_peer_close_defers_wake_until_admission_release() {
        let mut fixture = Fixture::new(|socket| socket);
        let connection = fixture.host.connection.clone();
        let guard = festerm_session::automation_admission_lock().unwrap();
        let wake = connection.close_under_admission(&guard);
        assert!(connection.is_closed());
        tokio::select! {
            biased;
            _ = fixture.wake.notified() => panic!("owner woke while admission was held"),
            _ = async {} => {}
        }

        super::super::wake_after_admission(guard, [wake]);
        tokio::time::timeout(Duration::from_secs(1), fixture.wake.notified())
            .await
            .expect("closed peer did not wake its owner");
        assert_eq!(fixture.host.step(), Ok(Step::Closed));
        assert!(fixture.cancelled.load(Ordering::Acquire));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn coordinator_stop_wakes_a_closed_direct_peer() {
        let mut fixture = Fixture::new(|socket| socket);
        let connection = fixture.host.connection.clone();
        let (mut coordinator, _app, _, _) = super::super::super::tests::fixture();
        coordinator.peers.get_mut(&1).unwrap().connection = connection.clone();

        coordinator.stop_mcp();
        assert!(connection.is_closed());
        tokio::time::timeout(Duration::from_secs(1), fixture.wake.notified())
            .await
            .expect("coordinator stop did not wake the direct owner");
        assert_eq!(fixture.host.step(), Ok(Step::Closed));
        assert!(fixture.cancelled.load(Ordering::Acquire));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn coordinator_stop_wakes_a_direct_peer_before_fingerprint_display() {
        let (mut coordinator, _app, _, _) = super::super::super::tests::fixture();
        coordinator.peers.clear();
        let wake = Wake::default();
        let wake_owner = {
            let wake = wake.clone();
            Arc::new(move || wake.notify())
        };
        let surface = coordinator.direct_verification_surface();
        let deadline = Instant::now() + Duration::from_secs(5);
        let verifier = loop {
            match surface.reserve(
                17,
                crate::application::WindowId::PRIMARY,
                coordinator.stop.clone(),
                coordinator.privacy_gate.clone(),
                Arc::new(|| {}),
                wake_owner.clone(),
            ) {
                Ok(verifier) => break verifier,
                Err(super::super::super::verification::Error::Busy)
                    if Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("could not reserve comparison: {error:?}"),
            }
        };
        let (host_socket, _client) = UnixStream::pair().unwrap();
        host_socket.set_nonblocking(true).unwrap();
        let owner_liveness = verifier.owner_liveness();
        let channel = SecureTransport::new(
            host_socket,
            Role::Application,
            verifier,
            Duration::from_secs(8),
            MAX_FRAME_DURATION,
        )
        .unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let (events, _receiver) = mpsc::sync_channel(1);
        let mut host = Peer::new(
            channel,
            Admission {
                open: Arc::new(AtomicBool::new(true)),
                cancelled: cancelled.clone(),
                fail_cleanup: false,
            },
            PeerContext {
                bootstrap: QualifiedBootstrap::synthetic(),
                key: 17,
                events,
                wake_ui: Arc::new(|| {}),
                stop: coordinator.stop.clone(),
                privacy_closed: coordinator.privacy_gate.closed_flag(),
                wake: wake.clone(),
            },
            owner_liveness,
        );
        assert_eq!(host.step(), Ok(Step::Blocked));
        assert!(coordinator.verification.view().is_none());
        assert!(coordinator.peers.is_empty());

        coordinator.stop_mcp();
        tokio::time::timeout(Duration::from_secs(1), wake.notified())
            .await
            .expect("pre-fingerprint direct peer did not wake on stop");
        assert_eq!(host.step(), Ok(Step::Closed));
        assert!(cancelled.load(Ordering::Acquire));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn direct_owner_closes_when_ui_owner_drops_before_fingerprint() {
        let ui_owner = verification::Owner::default();
        let stop = Arc::new(AtomicBool::new(false));
        let wake = Wake::default();
        let verifier = reserve_verifier(
            &ui_owner,
            stop.clone(),
            Arc::new(PrivacyGate::new(false)),
            &wake,
        );
        let (socket, client) = tokio::net::UnixStream::pair().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let (events, event_receiver) = mpsc::sync_channel(1);
        let (owner, mut status) = owner::Owner::new(
            socket,
            verifier,
            Admission {
                open: Arc::new(AtomicBool::new(true)),
                cancelled: cancelled.clone(),
                fail_cleanup: false,
            },
            PeerContext {
                bootstrap: QualifiedBootstrap::synthetic(),
                key: 17,
                events,
                wake_ui: Arc::new(|| {}),
                stop,
                privacy_closed: Arc::new(AtomicBool::new(false)),
                wake,
            },
            Duration::from_secs(8),
            MAX_FRAME_DURATION,
        )
        .unwrap();
        let mut worker = tokio::spawn(owner.run());
        tokio::task::yield_now().await;
        assert!(ui_owner.view().is_none());
        assert!(event_receiver.try_recv().is_err());
        drop(ui_owner);

        let result = match tokio::time::timeout(Duration::from_secs(2), &mut worker).await {
            Ok(result) => result.unwrap(),
            Err(_) => {
                worker.abort();
                assert!(worker.await.unwrap_err().is_cancelled());
                panic!("direct owner did not retire after UI-owner loss");
            }
        };
        assert_eq!(result, Ok(()));
        assert_eq!(status.wait().await, Ok(()));
        assert!(cancelled.load(Ordering::Acquire));
        tokio::time::timeout(Duration::from_secs(2), client.readable())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(client.try_read(&mut [0]).unwrap(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn direct_peer_closes_after_established_ui_owner_loss() {
        let mut ui_owner = verification::Owner::default();
        let stop = Arc::new(AtomicBool::new(false));
        let wake = Wake::default();
        let verifier = reserve_verifier(
            &ui_owner,
            stop.clone(),
            Arc::new(PrivacyGate::new(false)),
            &wake,
        );
        let owner_liveness = verifier.owner_liveness();
        let (host_socket, client_socket) = UnixStream::pair().unwrap();
        host_socket.set_nonblocking(true).unwrap();
        client_socket.set_nonblocking(true).unwrap();
        let host_channel = SecureTransport::new(
            host_socket,
            Role::Application,
            verifier,
            Duration::from_secs(8),
            MAX_FRAME_DURATION,
        )
        .unwrap();
        let client_native = Arc::new(NativeState::default());
        let mut client = SecureTransport::new(
            client_socket,
            Role::Client,
            Native(client_native.clone()),
            Duration::from_secs(8),
            MAX_FRAME_DURATION,
        )
        .unwrap();
        let (events, event_receiver) = mpsc::sync_channel(8);
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut host = Peer::new(
            host_channel,
            Admission {
                open: Arc::new(AtomicBool::new(true)),
                cancelled: cancelled.clone(),
                fail_cleanup: false,
            },
            PeerContext {
                bootstrap: QualifiedBootstrap::synthetic(),
                key: 17,
                events,
                wake_ui: Arc::new(|| {}),
                stop,
                privacy_closed: Arc::new(AtomicBool::new(false)),
                wake: wake.clone(),
            },
            owner_liveness,
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert_ne!(host.step().unwrap(), Step::Closed);
            client.poll(|| Ok(())).unwrap();
            ui_owner.advance();
            if let (Some(view), Some(client_code)) = (
                ui_owner.view(),
                client_native.displayed.lock().unwrap().clone(),
            ) {
                assert_eq!(view.fingerprint, client_code);
                match ui_owner.decide(view.window, view.generation, Some(&client_code)) {
                    Ok(()) => {
                        *client_native.decision.lock().unwrap() =
                            Some(view.fingerprint.as_bytes().try_into().unwrap());
                        break;
                    }
                    Err(verification::Error::Busy) => {}
                    Err(error) => panic!("comparison decision failed: {error:?}"),
                }
            }
            assert!(Instant::now() < deadline, "fingerprint exchange timed out");
            thread::sleep(Duration::from_millis(1));
        }
        let connection = loop {
            assert_ne!(host.step().unwrap(), Step::Closed);
            client.poll(|| Ok(())).unwrap();
            if let Ok(Event::Connected { connection, .. }) = event_receiver.try_recv() {
                break connection;
            }
            assert!(Instant::now() < deadline, "connection was not announced");
            thread::sleep(Duration::from_millis(1));
        };
        while !client.is_established() {
            assert_ne!(host.step().unwrap(), Step::Closed);
            client.poll(|| Ok(())).unwrap();
            assert!(Instant::now() < deadline, "client did not establish");
            thread::sleep(Duration::from_millis(1));
        }
        client
            .prepare_send(b"unadmitted", Instant::now() + MAX_FRAME_DURATION)
            .unwrap();
        loop {
            if matches!(client.poll(|| Ok(())).unwrap(), Progress::Sent) {
                break;
            }
            assert!(Instant::now() < deadline, "request was not written");
            thread::sleep(Duration::from_millis(1));
        }
        connection
            .send(
                b"undelivered".to_vec(),
                Instant::now() + MAX_FRAME_DURATION,
                &[],
            )
            .unwrap();
        tokio::select! {
            biased;
            _ = wake.notified() => {}
            _ = async {} => {}
        }
        drop(ui_owner);
        tokio::time::timeout(Duration::from_secs(2), wake.notified())
            .await
            .expect("UI-owner loss did not wake the established peer");
        assert_eq!(host.step(), Ok(Step::Closed));
        assert!(connection.is_closed());
        assert!(cancelled.load(Ordering::Acquire));
        assert!(matches!(
            event_receiver.try_recv(),
            Err(TryRecvError::Empty)
        ));
        loop {
            match client.poll(|| Ok(())) {
                Ok(Progress::Frame(_)) => panic!("queued frame was delivered after UI-owner loss"),
                Ok(_) => {}
                // Linux may reset a Unix socket closed with unread request bytes.
                Err(
                    secure_transport::Error::PeerClosed
                    | secure_transport::Error::Io(std::io::ErrorKind::ConnectionReset),
                ) => break,
                Err(error) => panic!("unexpected client error: {error}"),
            }
            assert!(Instant::now() < deadline, "client did not observe closure");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn direct_peer_rechecks_original_frame_after_admission_contention() {
        let mut fixture = Fixture::new(|socket| socket);
        let connection = fixture.establish();
        let valid = Arc::new(AtomicBool::new(true));
        fixture.open.store(false, Ordering::Release);
        connection
            .send_guarded(
                b"private".to_vec(),
                Instant::now() + MAX_FRAME_DURATION,
                &[],
                std::slice::from_ref(&valid),
            )
            .unwrap();
        assert_eq!(fixture.host.step().unwrap(), Step::Blocked);
        assert!(fixture.host.pending.is_some());
        assert!(!fixture.host.io_interest().writable);
        super::super::super::set_admission_flag(&valid, false);
        fixture.open.store(true, Ordering::Release);
        assert_eq!(
            fixture.host.step(),
            Err(Error::Channel(secure_transport::Error::Denied))
        );
        assert!(connection.is_closed());
        assert!(fixture.cancelled.load(Ordering::Acquire));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match fixture.client.poll(|| Ok(())) {
                Ok(Progress::Frame(_)) => panic!("revoked frame was delivered"),
                Ok(_) => {}
                Err(secure_transport::Error::PeerClosed) => break,
                Err(error) => panic!("unexpected client error: {error}"),
            }
            assert!(Instant::now() < deadline, "revoked peer did not close");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn direct_peer_closes_if_incoming_admission_is_temporarily_unavailable() {
        let mut fixture = Fixture::new(|socket| socket);
        let connection = fixture.establish();
        fixture.open.store(false, Ordering::Release);
        fixture
            .client
            .prepare_send(b"cancel", Instant::now() + MAX_FRAME_DURATION)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            fixture.client.poll(|| Ok(())).unwrap();
            match fixture.host.step() {
                Err(Error::Admission) => break,
                Ok(Step::Advanced | Step::Blocked) => {}
                other => panic!("unexpected host result: {other:?}"),
            }
            assert!(Instant::now() < deadline, "incoming frame was not refused");
            thread::sleep(Duration::from_millis(1));
        }
        assert!(connection.is_closed());
        assert!(fixture.events.try_recv().is_err());
    }

    struct CutOnWrite {
        stream: UnixStream,
        armed: Arc<AtomicBool>,
        valid: Arc<AtomicBool>,
        cut: Arc<AtomicBool>,
    }

    impl Read for CutOnWrite {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.stream.read(bytes)
        }
    }

    impl Write for CutOnWrite {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if !self.armed.load(Ordering::Acquire) {
                return self.stream.write(bytes);
            }
            let count = self.stream.write(&bytes[..bytes.len().min(1)])?;
            if count != 0 {
                self.valid.store(false, Ordering::Release);
                self.cut.store(true, Ordering::Release);
            }
            Ok(count)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.stream.flush()
        }
    }

    #[test]
    fn direct_peer_never_finishes_a_partially_written_revoked_frame() {
        let armed = Arc::new(AtomicBool::new(false));
        let valid = Arc::new(AtomicBool::new(true));
        let cut = Arc::new(AtomicBool::new(false));
        let mut fixture = Fixture::new(|stream| CutOnWrite {
            stream,
            armed: armed.clone(),
            valid: valid.clone(),
            cut: cut.clone(),
        });
        let connection = fixture.establish();
        connection
            .send_guarded(
                b"private response".to_vec(),
                Instant::now() + MAX_FRAME_DURATION,
                &[],
                std::slice::from_ref(&valid),
            )
            .unwrap();
        armed.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !cut.load(Ordering::Acquire) {
            fixture.pump();
            assert!(Instant::now() < deadline, "partial write did not happen");
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            fixture.host.step(),
            Err(Error::Channel(secure_transport::Error::Denied))
        );
        assert!(connection.is_closed());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match fixture.client.poll(|| Ok(())) {
                Ok(Progress::Frame(_)) => panic!("partial revoked frame was delivered"),
                Ok(_) => {}
                Err(secure_transport::Error::PeerClosed) => break,
                Err(error) => panic!("unexpected client error: {error}"),
            }
            assert!(Instant::now() < deadline, "partial peer did not close");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn direct_peer_stops_partial_frame_after_observed_privacy_loss() {
        let armed = Arc::new(AtomicBool::new(false));
        let unused_valid = Arc::new(AtomicBool::new(true));
        let privacy_closed = Arc::new(AtomicBool::new(false));
        let mut fixture = Fixture::new(|stream| CutOnWrite {
            stream,
            armed: armed.clone(),
            valid: unused_valid.clone(),
            cut: privacy_closed.clone(),
        });
        fixture.host.privacy_closed = privacy_closed.clone();
        let connection = fixture.establish();
        connection
            .send(
                b"private response".to_vec(),
                Instant::now() + MAX_FRAME_DURATION,
                &[],
            )
            .unwrap();
        armed.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !privacy_closed.load(Ordering::Acquire) {
            fixture.pump();
            assert!(Instant::now() < deadline, "partial write did not happen");
            thread::sleep(Duration::from_millis(1));
        }
        assert!(!fixture.host.stop.load(Ordering::Acquire));
        assert_eq!(fixture.host.step(), Ok(Step::Closed));
        assert!(connection.is_closed());
        loop {
            match fixture.client.poll(|| Ok(())) {
                Ok(Progress::Frame(_)) => panic!("partial frame was delivered after privacy loss"),
                Ok(_) => {}
                Err(
                    secure_transport::Error::PeerClosed
                    | secure_transport::Error::Io(std::io::ErrorKind::ConnectionReset),
                ) => break,
                Err(error) => panic!("unexpected client error: {error}"),
            }
            assert!(Instant::now() < deadline, "privacy-cut peer did not close");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn direct_peer_reports_native_admission_cleanup_failure() {
        let mut fixture = Fixture::new(|socket| socket);
        fixture.host.admission.as_mut().unwrap().fail_cleanup = true;
        assert_eq!(fixture.host.finish(), Err(Error::Cleanup));
        assert_eq!(fixture.host.finish(), Err(Error::Cleanup));
        assert!(fixture.cancelled.load(Ordering::Acquire));
        assert!(fixture.host_native.cancelled.load(Ordering::Acquire));
    }

    fn new_owner(
        socket: tokio::net::UnixStream,
        stop: Arc<AtomicBool>,
        wake: Wake,
        native: Arc<NativeState>,
        cancelled: Arc<AtomicBool>,
        fail_cleanup: bool,
        bootstrap_duration: Duration,
    ) -> Result<(owner::Owner<Native, Admission>, owner::Status), Error> {
        let (events, _) = mpsc::sync_channel(1);
        owner::Owner::new(
            socket,
            Native(native),
            Admission {
                open: Arc::new(AtomicBool::new(true)),
                cancelled,
                fail_cleanup,
            },
            PeerContext {
                bootstrap: QualifiedBootstrap::synthetic(),
                key: 17,
                events,
                wake_ui: Arc::new(|| {}),
                stop,
                privacy_closed: Arc::new(AtomicBool::new(false)),
                wake,
            },
            bootstrap_duration,
            MAX_FRAME_DURATION,
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn direct_owner_wakes_on_stop_and_reports_cleanup() {
        let (socket, client) = tokio::net::UnixStream::pair().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let wake = Wake::default();
        let native = Arc::new(NativeState::default());
        let cancelled = Arc::new(AtomicBool::new(false));
        let (owner, mut status) = new_owner(
            socket,
            stop.clone(),
            wake.clone(),
            native.clone(),
            cancelled.clone(),
            false,
            Duration::from_secs(8),
        )
        .unwrap();
        let worker = tokio::spawn(owner.run());
        tokio::task::yield_now().await;
        super::super::super::set_admission_flag(&stop, true);
        wake.notify();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), worker)
                .await
                .unwrap()
                .unwrap(),
            Ok(())
        );
        assert_eq!(status.wait().await, Ok(()));
        assert!(native.cancelled.load(Ordering::Acquire));
        assert!(cancelled.load(Ordering::Acquire));
        tokio::time::timeout(Duration::from_secs(5), client.readable())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(client.try_read(&mut [0]).unwrap(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn direct_owner_expires_without_socket_activity() {
        let (socket, _client) = tokio::net::UnixStream::pair().unwrap();
        let native = Arc::new(NativeState::default());
        let cancelled = Arc::new(AtomicBool::new(false));
        let (owner, mut status) = new_owner(
            socket,
            Arc::new(AtomicBool::new(false)),
            Wake::default(),
            native.clone(),
            cancelled.clone(),
            false,
            Duration::from_millis(100),
        )
        .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), owner.run())
            .await
            .unwrap();
        assert_eq!(
            result,
            Err(Error::Channel(secure_transport::Error::Deadline))
        );
        assert_eq!(status.wait().await, result);
        assert!(native.cancelled.load(Ordering::Acquire));
        assert!(cancelled.load(Ordering::Acquire));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn direct_owner_reports_cleanup_after_task_cancellation() {
        for fail_cleanup in [false, true] {
            let (socket, _client) = tokio::net::UnixStream::pair().unwrap();
            let native = Arc::new(NativeState::default());
            let cancelled = Arc::new(AtomicBool::new(false));
            let (owner, mut status) = new_owner(
                socket,
                Arc::new(AtomicBool::new(false)),
                Wake::default(),
                native.clone(),
                cancelled.clone(),
                fail_cleanup,
                Duration::from_secs(8),
            )
            .unwrap();
            let worker = tokio::spawn(owner.run());
            tokio::task::yield_now().await;
            worker.abort();
            assert!(tokio::time::timeout(Duration::from_secs(5), worker)
                .await
                .unwrap()
                .unwrap_err()
                .is_cancelled());
            assert_eq!(
                status.wait().await,
                Err(if fail_cleanup {
                    Error::Cleanup
                } else {
                    Error::OwnerInterrupted
                })
            );
            assert!(native.cancelled.load(Ordering::Acquire));
            assert!(cancelled.load(Ordering::Acquire));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn direct_owner_retires_both_boundaries_if_construction_fails() {
        let (socket, _client) = tokio::net::UnixStream::pair().unwrap();
        let native = Arc::new(NativeState::default());
        let cancelled = Arc::new(AtomicBool::new(false));
        assert!(matches!(
            new_owner(
                socket,
                Arc::new(AtomicBool::new(false)),
                Wake::default(),
                native.clone(),
                cancelled.clone(),
                false,
                Duration::ZERO,
            ),
            Err(Error::Channel(secure_transport::Error::Protocol))
        ));
        assert!(native.cancelled.load(Ordering::Acquire));
        assert!(cancelled.load(Ordering::Acquire));
    }

    struct ContendedAdmission {
        open: Arc<AtomicBool>,
        blocked: Arc<Notify>,
    }

    impl NativeAdmission for ContendedAdmission {
        type Guard<'a> = ();

        fn try_admit(&self) -> io::Result<Self::Guard<'_>> {
            if self.open.load(Ordering::Acquire) {
                Ok(())
            } else {
                self.blocked.notify_one();
                Err(io::ErrorKind::WouldBlock.into())
            }
        }

        fn cancel(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn direct_owner_waits_for_admission_wake_before_writing() {
        use festerm_automation_mcp_client::DirectClientTransport;
        use rmcp::transport::Transport;

        let (host_socket, client_socket) = tokio::net::UnixStream::pair().unwrap();
        let host_native = Arc::new(NativeState::default());
        let client_native = Arc::new(NativeState::default());
        let open = Arc::new(AtomicBool::new(true));
        let blocked = Arc::new(Notify::new());
        let host_wake = Wake::default();
        let (events_tx, events_rx) = mpsc::sync_channel(8);
        let (host, mut host_status) = owner::Owner::new(
            host_socket,
            Native(host_native.clone()),
            ContendedAdmission {
                open: open.clone(),
                blocked: blocked.clone(),
            },
            PeerContext {
                bootstrap: QualifiedBootstrap::synthetic(),
                key: 17,
                events: events_tx,
                wake_ui: Arc::new(|| {}),
                stop: Arc::new(AtomicBool::new(false)),
                privacy_closed: Arc::new(AtomicBool::new(false)),
                wake: host_wake.clone(),
            },
            Duration::from_secs(8),
            MAX_FRAME_DURATION,
        )
        .unwrap();
        let worker = tokio::spawn(host.run());
        let (wake_tx, wake_rx) = mpsc::sync_channel(1);
        let decision = client_native.clone();
        let (mut client, mut client_status) = DirectClientTransport::new(
            client_socket,
            Duration::from_secs(8),
            MAX_FRAME_DURATION,
            move |wake| {
                wake_tx.send(wake).unwrap();
                (Native(decision), ClientAdmission)
            },
        )
        .unwrap();
        let client_wake = wake_rx.recv().unwrap();
        let connection = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let host_code = host_native.displayed.lock().unwrap().clone();
                let client_code = client_native.displayed.lock().unwrap().clone();
                if let (Some(host_code), Some(client_code)) = (host_code, client_code) {
                    assert_eq!(host_code, client_code);
                    *client_native.decision.lock().unwrap() =
                        Some(host_code.as_bytes().try_into().unwrap());
                    *host_native.decision.lock().unwrap() =
                        Some(client_code.as_bytes().try_into().unwrap());
                    host_wake.notify();
                    client_wake.wake();
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            loop {
                if let Ok(Event::Connected { connection, .. }) = events_rx.try_recv() {
                    break connection;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("direct host did not establish");

        open.store(false, Ordering::Release);
        connection
            .send(
                br#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#.to_vec(),
                Instant::now() + MAX_FRAME_DURATION,
                &[],
            )
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), blocked.notified())
            .await
            .expect("write did not encounter admission contention");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), client.receive())
                .await
                .is_err()
        );
        open.store(true, Ordering::Release);
        host_wake.notify();
        let message = tokio::time::timeout(Duration::from_secs(5), client.receive())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(message).unwrap()["method"],
            "notifications/tools/list_changed"
        );
        connection.close();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), worker)
                .await
                .unwrap()
                .unwrap(),
            Ok(())
        );
        assert_eq!(host_status.wait().await, Ok(()));
        let client_result = client.close().await;
        assert!(matches!(
            client_result,
            Ok(())
                | Err(festerm_automation_mcp_client::Error::Channel(
                    secure_transport::Error::PeerClosed
                ))
        ));
        assert_eq!(client_status.wait().await, client_result);
    }

    pub(super) struct ClientAdmission;

    impl festerm_automation_mcp_client::ClientWriteAdmission for ClientAdmission {
        type Guard<'a> = ();

        fn try_admit(&self) -> io::Result<Self::Guard<'_>> {
            Ok(())
        }

        fn cancel(&mut self) -> Result<(), festerm_automation_mcp_client::AdmissionCleanupFailed> {
            Ok(())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn direct_sdk_observation_reaches_the_real_coordinator() {
        use eframe::egui;
        use festerm_automation_mcp_client::DirectClientTransport;
        use rmcp::{
            model::{CallToolRequestParams, ClientConfig, ProtocolVersion},
            ServiceExt,
        };
        use serde_json::json;

        let (app, tab, mut coordinator, events) = observation_fixture();
        let (client_socket, host_socket) = tokio::net::UnixStream::pair().unwrap();
        let host_native = Arc::new(NativeState::default());
        let client_native = Arc::new(NativeState::default());
        let host_wake = Wake::default();
        let stop = coordinator.stop.clone();
        let (host, mut host_status) = owner::Owner::new(
            host_socket,
            Native(host_native.clone()),
            Admission {
                open: Arc::new(AtomicBool::new(true)),
                cancelled: Arc::new(AtomicBool::new(false)),
                fail_cleanup: false,
            },
            PeerContext {
                bootstrap: QualifiedBootstrap::synthetic(),
                key: 17,
                events,
                wake_ui: Arc::new(|| {}),
                stop: stop.clone(),
                privacy_closed: coordinator.privacy_gate.closed_flag(),
                wake: host_wake.clone(),
            },
            Duration::from_secs(8),
            MAX_FRAME_DURATION,
        )
        .unwrap();
        let worker = tokio::spawn(host.run());

        let (wake_tx, wake_rx) = mpsc::sync_channel(1);
        let client_decision = client_native.clone();
        let (transport, mut status) = DirectClientTransport::new(
            client_socket,
            Duration::from_secs(8),
            MAX_FRAME_DURATION,
            move |wake| {
                wake_tx.send(wake).unwrap();
                (Native(client_decision), ClientAdmission)
            },
        )
        .unwrap();
        let client_wake = wake_rx.recv().unwrap();
        let (approval, mut approval_requests) = tokio::sync::mpsc::channel::<()>(1);
        let (approved, mut approval_results) = tokio::sync::mpsc::channel::<()>(1);
        let finished = Arc::new(AtomicBool::new(false));
        let sdk_finished = finished.clone();
        let sdk = async move {
            let mut config = ClientConfig::default();
            config.protocol_version = ProtocolVersion::V_2025_11_25;
            let client = config.serve(transport).await.unwrap();
            let tools = client.list_all_tools().await.unwrap();
            assert!(tools
                .iter()
                .any(|tool| tool.name == "festerm_sessions_list"));
            let request = || {
                CallToolRequestParams::new("festerm_sessions_list")
                    .with_arguments(serde_json::Map::new())
            };
            let denied = client.call_tool(request()).await.unwrap();
            assert_eq!(denied.is_error, Some(true));
            assert_eq!(denied.structured_content.unwrap()["code"], "NOT_AUTHORIZED");
            approval.send(()).await.unwrap();
            approval_results.recv().await.unwrap();
            let listed = client.call_tool(request()).await.unwrap();
            let listed = listed.structured_content.unwrap();
            let sessions = listed["data"]["sessions"].as_array().unwrap();
            assert_eq!(sessions.len(), 1);
            let reference = sessions[0]["session_ref"].as_str().unwrap();
            let screen = client
                .call_tool(
                    CallToolRequestParams::new("festerm_terminal_read_screen").with_arguments(
                        json!({"session_ref": reference, "max_bytes": 4096})
                            .as_object()
                            .unwrap()
                            .clone(),
                    ),
                )
                .await
                .unwrap();
            assert!(screen.structured_content.unwrap()["data"]["text"]
                .as_str()
                .unwrap()
                .starts_with("approved source"));
            client.cancel().await.unwrap();
            sdk_finished.store(true, Ordering::Release);
        };
        let compare = async {
            loop {
                let host_code = host_native.displayed.lock().unwrap().clone();
                let client_code = client_native.displayed.lock().unwrap().clone();
                if let (Some(host_code), Some(client_code)) = (host_code, client_code) {
                    assert_eq!(host_code, client_code);
                    *client_native.decision.lock().unwrap() =
                        Some(host_code.as_bytes().try_into().unwrap());
                    *host_native.decision.lock().unwrap() =
                        Some(client_code.as_bytes().try_into().unwrap());
                    client_wake.wake();
                    host_wake.notify();
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        };
        let coordinator_loop = async {
            let context = egui::Context::default();
            loop {
                coordinator.tick(&context, &[&app]);
                if approval_requests.try_recv().is_ok() {
                    let target = coordinator.targets[&tab.chip_id()].reference;
                    super::super::super::tests::approve_for_test(
                        &mut coordinator,
                        17,
                        vec![target],
                    )
                    .unwrap();
                    approved.try_send(()).unwrap();
                }
                if finished.load(Ordering::Acquire) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        };
        let completed = tokio::time::timeout(Duration::from_secs(20), async {
            tokio::join!(compare, coordinator_loop, sdk);
        })
        .await;
        super::super::super::set_admission_flag(&stop, true);
        host_wake.notify();
        let host_result = tokio::time::timeout(Duration::from_secs(5), worker)
            .await
            .expect("direct host did not stop")
            .unwrap();
        assert!(matches!(
            host_result,
            Ok(()) | Err(Error::Channel(secure_transport::Error::PeerClosed))
        ));
        assert_eq!(host_status.wait().await, host_result);
        completed.expect("direct SDK coordinator exchange timed out");
        assert_eq!(status.wait().await, Ok(()));
    }

    pub(super) fn observation_fixture() -> (
        super::super::super::FesTermApp,
        crate::tabs::TabId,
        super::super::super::Coordinator,
        mpsc::SyncSender<Event>,
    ) {
        use super::super::super::{Broker, Coordinator, FesTermApp, PrivacyGate};

        let (mut app, tab) = FesTermApp::automation_fixture();
        let settings = festerm_config::InterfaceSettings::DEFAULT.with_mcp_enabled(true);
        app.adopt_broadcast_configuration(
            festerm_config::Configuration::empty()
                .with_interface_settings(settings)
                .unwrap(),
        );
        let mut coordinator = Coordinator::default();
        coordinator.broker = Some(Broker::new([3; 16]));
        coordinator.stop = Arc::new(AtomicBool::new(false));
        coordinator.privacy_gate = Arc::new(PrivacyGate::new(false));
        assert!(coordinator.privacy_gate.bind_mcp(&coordinator.stop));
        coordinator.enabled = true;
        let (server, events) = super::super::test_server(coordinator.stop.clone());
        coordinator.server = Some(server);
        (app, tab, coordinator, events)
    }
}
