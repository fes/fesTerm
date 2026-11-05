//! Native Settings comparison for a dormant direct MCP connection.

use super::PrivacyGate;
use crate::application::WindowId;
use festerm_automation_mcp::secure_transport::{
    NativeCleanupFailed, NativeDecision, NativeUnavailable, TrustedNativeVerification,
    FINGERPRINT_TEXT_BYTES,
};
use std::{
    mem,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
        Arc,
    },
};

type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Error {
    Busy,
    Unavailable,
}

struct Liveness {
    alive: AtomicBool,
    pending: AtomicBool,
    stop: Arc<AtomicBool>,
    privacy: Arc<PrivacyGate>,
    quarantined: Arc<AtomicBool>,
}

impl Liveness {
    fn alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
            && !self.quarantined.load(Ordering::Acquire)
            && !self.stop.load(Ordering::Acquire)
            && !self.privacy.is_closed()
    }

    fn current(&self) -> bool {
        self.alive() && self.pending.load(Ordering::Acquire)
    }

    fn invalidate(&self) -> bool {
        let was_alive = self.alive.swap(false, Ordering::AcqRel);
        self.pending.store(false, Ordering::Release);
        was_alive
    }
}

#[derive(Clone)]
pub(crate) struct View {
    pub(crate) generation: u64,
    pub(crate) channel: u64,
    pub(crate) window: WindowId,
    pub(crate) fingerprint: String,
    live: Arc<Liveness>,
}

impl View {
    pub(crate) fn is_current(&self) -> bool {
        self.live.current()
    }

    pub(super) fn same_presentation(&self, other: &Self) -> bool {
        self.generation == other.generation
            && self.channel == other.channel
            && self.window == other.window
            && self.fingerprint == other.fingerprint
            && self.is_current() == other.is_current()
    }

    #[cfg(test)]
    pub(crate) fn for_test(window: WindowId) -> Self {
        Self {
            generation: 1,
            channel: 7,
            window,
            fingerprint: "a".repeat(FINGERPRINT_TEXT_BYTES),
            live: Arc::new(Liveness {
                alive: AtomicBool::new(true),
                pending: AtomicBool::new(true),
                stop: Arc::new(AtomicBool::new(false)),
                privacy: Arc::new(PrivacyGate::new(false)),
                quarantined: Arc::new(AtomicBool::new(false)),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn retire_for_test(&self) {
        self.live.invalidate();
    }
}

enum Decision {
    Denied,
    Compared([u8; FINGERPRINT_TEXT_BYTES]),
}

struct Prompt {
    view: View,
    decision: SyncSender<Decision>,
    wake_owner: Wake,
}

struct Reservation {
    live: Arc<Liveness>,
    wake_owner: Wake,
}

#[cfg(test)]
struct AdvanceTestGate {
    entered: SyncSender<()>,
    resume: Receiver<()>,
}

#[cfg(test)]
thread_local! {
    static ADVANCE_TEST_GATE: std::cell::RefCell<Option<AdvanceTestGate>> = const {
        std::cell::RefCell::new(None)
    };
}

struct Shared {
    pending: SyncSender<Prompt>,
    // A cancelled verifier may leave its prompt queued after releasing the reservation.
    pending_occupied: AtomicBool,
    reservations: SyncSender<Reservation>,
    reserved: AtomicBool,
    owner_alive: AtomicBool,
    generation: AtomicU64,
    quarantined: Arc<AtomicBool>,
}

#[derive(Clone)]
pub(super) struct Surface(Arc<Shared>);

pub(super) struct Owner {
    surface: Surface,
    pending: Receiver<Prompt>,
    reservations: Receiver<Reservation>,
    active: Option<Reservation>,
    current: Option<Prompt>,
}

impl Default for Owner {
    fn default() -> Self {
        let (sender, pending) = mpsc::sync_channel(1);
        let (reservation_sender, reservations) = mpsc::sync_channel(1);
        Self {
            surface: Surface(Arc::new(Shared {
                pending: sender,
                pending_occupied: AtomicBool::new(false),
                reservations: reservation_sender,
                reserved: AtomicBool::new(false),
                owner_alive: AtomicBool::new(true),
                generation: AtomicU64::new(0),
                quarantined: Arc::new(AtomicBool::new(false)),
            })),
            pending,
            reservations,
            active: None,
            current: None,
        }
    }
}

impl Owner {
    pub(super) fn surface(&self) -> Surface {
        self.surface.clone()
    }

    pub(super) fn quarantined(&self) -> bool {
        self.surface.0.quarantined.load(Ordering::Acquire)
    }

    pub(super) fn advance(&mut self) {
        self.dismiss_retired_prompt();
        self.retire_dead_reservation();
        #[cfg(test)]
        ADVANCE_TEST_GATE.with(|slot| {
            if let Some(gate) = slot.borrow_mut().take() {
                gate.entered
                    .send(())
                    .expect("enter comparison advance gate");
                gate.resume
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("resume comparison advance");
            }
        });
        match self.reservations.try_recv() {
            Ok(reservation) if !reservation.live.alive() => self.retire(reservation),
            Ok(reservation) => {
                // Replacement can arrive after the initial liveness sweep.
                self.retire_dead_reservation();
                if self.active.is_none() {
                    self.active = Some(reservation);
                } else {
                    self.retire(reservation);
                    self.quarantine();
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.quarantine(),
        }
        match self.take_prompt() {
            Ok(prompt) if prompt.view.is_current() => {
                self.dismiss_retired_prompt();
                if self.current.is_some() {
                    self.dismiss(prompt);
                    self.quarantine();
                } else {
                    self.current = Some(prompt);
                }
            }
            Ok(prompt) => self.dismiss(prompt),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.quarantine(),
        }
    }

    pub(super) fn view(&self) -> Option<View> {
        self.current
            .as_ref()
            .filter(|prompt| prompt.view.is_current())
            .map(|prompt| prompt.view.clone())
    }

    pub(super) fn decide(
        &mut self,
        window: WindowId,
        generation: u64,
        peer: Option<&str>,
    ) -> Result<(), Error> {
        let _admission = admission()?;
        let prompt = self.current.as_ref().ok_or(Error::Unavailable)?;
        if prompt.view.window != window
            || prompt.view.generation != generation
            || !prompt.view.is_current()
        {
            return Err(Error::Unavailable);
        }
        let decision = match peer {
            Some(peer) => {
                let mut bytes = fingerprint(peer).ok_or(Error::Unavailable)?;
                bytes.make_ascii_lowercase();
                Decision::Compared(bytes)
            }
            None => Decision::Denied,
        };
        if prompt
            .view
            .live
            .pending
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(Error::Unavailable);
        }
        if prompt.decision.try_send(decision).is_err() {
            prompt.view.live.invalidate();
            self.quarantine();
            return Err(Error::Unavailable);
        }
        drop(_admission);
        if !self.surface.wake(&prompt.wake_owner) {
            prompt.view.live.invalidate();
            return Err(Error::Unavailable);
        }
        Ok(())
    }

    pub(super) fn revoke(&mut self) {
        if let Some(prompt) = self.current.take() {
            self.dismiss(prompt);
        }
        if let Ok(prompt) = self.take_prompt() {
            self.dismiss(prompt);
        }
        if let Some(reservation) = self.active.take() {
            self.retire(reservation);
        }
        if let Ok(reservation) = self.reservations.try_recv() {
            self.retire(reservation);
        }
    }

    fn dismiss(&self, prompt: Prompt) {
        if prompt.view.live.invalidate() {
            self.surface.wake(&prompt.wake_owner);
        }
    }

    fn dismiss_retired_prompt(&mut self) {
        if self
            .current
            .as_ref()
            .is_some_and(|prompt| !prompt.view.live.alive())
        {
            let prompt = self.current.take().expect("current prompt was checked");
            self.dismiss(prompt);
        }
    }

    fn retire_dead_reservation(&mut self) {
        if self
            .active
            .as_ref()
            .is_some_and(|reservation| !reservation.live.alive())
        {
            let reservation = self.active.take().expect("active reservation was checked");
            self.retire(reservation);
        }
    }

    fn take_prompt(&self) -> Result<Prompt, TryRecvError> {
        let prompt = self.pending.try_recv()?;
        self.surface
            .0
            .pending_occupied
            .store(false, Ordering::Release);
        Ok(prompt)
    }

    fn retire(&self, reservation: Reservation) {
        if reservation.live.invalidate() {
            self.surface.wake(&reservation.wake_owner);
        }
    }

    fn quarantine(&self) {
        self.surface.0.quarantined.store(true, Ordering::Release);
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.surface.0.owner_alive.store(false, Ordering::Release);
        self.revoke();
    }
}

impl Surface {
    pub(super) fn reserve(
        &self,
        channel: u64,
        window: WindowId,
        stop: Arc<AtomicBool>,
        privacy: Arc<PrivacyGate>,
        wake_ui: Wake,
        wake_owner: Wake,
    ) -> Result<Verifier, Error> {
        if window != WindowId::PRIMARY
            || !self.0.owner_alive.load(Ordering::Acquire)
            || self.0.quarantined.load(Ordering::Acquire)
        {
            return Err(Error::Unavailable);
        }
        let _admission = admission()?;
        if stop.load(Ordering::Acquire)
            || privacy.is_closed()
            || !self.0.owner_alive.load(Ordering::Acquire)
            || self.0.quarantined.load(Ordering::Acquire)
        {
            return Err(Error::Unavailable);
        }
        self.0
            .reserved
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::Busy)?;
        if self.0.pending_occupied.load(Ordering::Acquire) {
            self.0.reserved.store(false, Ordering::Release);
            return Err(Error::Busy);
        }
        let generation =
            match self
                .0
                .generation
                .try_update(Ordering::AcqRel, Ordering::Acquire, |generation| {
                    generation.checked_add(1)
                }) {
                Ok(generation) => generation + 1,
                Err(_) => {
                    self.0.quarantined.store(true, Ordering::Release);
                    return Err(Error::Unavailable);
                }
            };
        let live = Arc::new(Liveness {
            alive: AtomicBool::new(true),
            pending: AtomicBool::new(false),
            stop,
            privacy,
            quarantined: self.0.quarantined.clone(),
        });
        let reservation = Reservation {
            live: live.clone(),
            wake_owner: wake_owner.clone(),
        };
        match self.0.reservations.try_send(reservation) {
            Ok(()) => {}
            Err(TrySendError::Full(reservation)) => {
                self.0.reserved.store(false, Ordering::Release);
                drop(_admission);
                drop(reservation);
                return Err(Error::Busy);
            }
            Err(TrySendError::Disconnected(reservation)) => {
                self.0.quarantined.store(true, Ordering::Release);
                self.0.reserved.store(false, Ordering::Release);
                drop(_admission);
                drop(reservation);
                return Err(Error::Unavailable);
            }
        }
        drop(_admission);
        if !self.0.owner_alive.load(Ordering::Acquire) {
            live.invalidate();
            self.0.reserved.store(false, Ordering::Release);
            return Err(Error::Unavailable);
        }
        let (decision, decisions) = mpsc::sync_channel(1);
        Ok(Verifier {
            surface: self.clone(),
            channel,
            generation,
            window,
            live,
            decision: Some(decision),
            decisions,
            wake_ui,
            wake_owner,
            displayed: false,
            cancelled: false,
        })
    }

    fn wake(&self, wake: &Wake) -> bool {
        match catch_unwind(AssertUnwindSafe(|| wake())) {
            Ok(()) => true,
            Err(payload) => {
                self.0.quarantined.store(true, Ordering::Release);
                if let Err(payload) = catch_unwind(AssertUnwindSafe(|| drop(payload))) {
                    mem::forget(payload);
                }
                false
            }
        }
    }
}

pub(super) struct Verifier {
    surface: Surface,
    channel: u64,
    generation: u64,
    window: WindowId,
    live: Arc<Liveness>,
    decision: Option<SyncSender<Decision>>,
    decisions: Receiver<Decision>,
    wake_ui: Wake,
    wake_owner: Wake,
    displayed: bool,
    cancelled: bool,
}

impl TrustedNativeVerification for Verifier {
    fn show_local_fingerprint(&mut self, value: &str) -> Result<(), NativeUnavailable> {
        if self.displayed || !self.live.alive() || fingerprint(value).is_none() {
            return Err(NativeUnavailable);
        }
        self.displayed = true;
        self.live.pending.store(true, Ordering::Release);
        let prompt = Prompt {
            view: View {
                generation: self.generation,
                channel: self.channel,
                window: self.window,
                fingerprint: value.to_owned(),
                live: self.live.clone(),
            },
            decision: self.decision.take().ok_or(NativeUnavailable)?,
            wake_owner: self.wake_owner.clone(),
        };
        if self
            .surface
            .0
            .pending_occupied
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            self.live.invalidate();
            return Err(NativeUnavailable);
        }
        match self.surface.0.pending.try_send(prompt) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.surface
                    .0
                    .pending_occupied
                    .store(false, Ordering::Release);
                self.live.invalidate();
                self.surface.0.quarantined.store(true, Ordering::Release);
                return Err(NativeUnavailable);
            }
            Err(TrySendError::Disconnected(_)) => {
                self.surface
                    .0
                    .pending_occupied
                    .store(false, Ordering::Release);
                self.live.invalidate();
                self.surface.0.quarantined.store(true, Ordering::Release);
                return Err(NativeUnavailable);
            }
        }
        if !self.surface.wake(&self.wake_ui) {
            self.live.invalidate();
            return Err(NativeUnavailable);
        }
        Ok(())
    }

    fn poll_decision(&mut self) -> NativeDecision {
        // The native decision and every physical write are admission-fenced;
        // polling must not spin on that non-reentrant lock.
        if !self.live.alive() {
            return NativeDecision::Denied;
        }
        match self.decisions.try_recv() {
            Ok(Decision::Compared(value)) if self.live.alive() => {
                NativeDecision::ComparedPeer(value)
            }
            Ok(Decision::Denied) | Ok(Decision::Compared(_)) => NativeDecision::Denied,
            Err(TryRecvError::Empty) => NativeDecision::Pending,
            Err(TryRecvError::Disconnected) => NativeDecision::Denied,
        }
    }

    fn begin_retirement(&mut self) -> Result<(), NativeCleanupFailed> {
        self.live.invalidate();
        if self.surface.wake(&self.wake_ui) {
            Ok(())
        } else {
            Err(NativeCleanupFailed)
        }
    }

    fn cancel(&mut self) -> Result<(), NativeCleanupFailed> {
        if self.cancelled {
            return if self.surface.0.quarantined.load(Ordering::Acquire) {
                Err(NativeCleanupFailed)
            } else {
                Ok(())
            };
        }
        self.live.invalidate();
        self.cancelled = true;
        if !self.surface.wake(&self.wake_ui) || self.surface.0.quarantined.load(Ordering::Acquire) {
            return Err(NativeCleanupFailed);
        }
        self.surface.0.reserved.store(false, Ordering::Release);
        Ok(())
    }
}

impl Drop for Verifier {
    fn drop(&mut self) {
        if !self.cancelled {
            let _ = self.cancel();
        }
    }
}

pub(crate) fn valid_fingerprint(value: &str) -> bool {
    fingerprint(value).is_some()
}

fn fingerprint(value: &str) -> Option<[u8; FINGERPRINT_TEXT_BYTES]> {
    let bytes: [u8; FINGERPRINT_TEXT_BYTES] = value.as_bytes().try_into().ok()?;
    bytes.iter().all(u8::is_ascii_hexdigit).then_some(bytes)
}

fn admission() -> Result<festerm_session::AutomationAdmissionGuard, Error> {
    festerm_session::try_automation_admission_lock().map_err(|error| match error {
        festerm_session::AutomationAdmissionTryError::Busy => Error::Busy,
        festerm_session::AutomationAdmissionTryError::Poisoned => Error::Unavailable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::atomic::AtomicUsize,
        thread,
        time::{Duration, Instant},
    };

    fn fixture() -> (Owner, Verifier, Arc<AtomicUsize>, Arc<AtomicBool>) {
        let owner = Owner::default();
        let stop = Arc::new(AtomicBool::new(false));
        let privacy = Arc::new(PrivacyGate::new(false));
        let wakes = Arc::new(AtomicUsize::new(0));
        let wake_owner = {
            let wakes = wakes.clone();
            Arc::new(move || {
                wakes.fetch_add(1, Ordering::Relaxed);
            }) as Wake
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let verifier = loop {
            match owner.surface().reserve(
                7,
                WindowId::PRIMARY,
                stop.clone(),
                privacy.clone(),
                Arc::new(|| {}),
                wake_owner.clone(),
            ) {
                Ok(verifier) => break verifier,
                Err(Error::Busy) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("could not reserve comparison: {error:?}"),
            }
        };
        (owner, verifier, wakes, stop)
    }

    fn display(verifier: &mut Verifier, value: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match verifier.show_local_fingerprint(value) {
                Ok(()) => return,
                Err(_) if !verifier.displayed && Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(_) => panic!("could not display comparison"),
            }
        }
    }

    fn decide(
        owner: &mut Owner,
        window: WindowId,
        generation: u64,
        peer: Option<&str>,
    ) -> Result<(), Error> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let result = owner.decide(window, generation, peer);
            if result != Err(Error::Busy) || Instant::now() >= deadline {
                return result;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn decision(verifier: &mut Verifier) -> NativeDecision {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match verifier.poll_decision() {
                NativeDecision::Pending if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(1));
                }
                result => return result,
            }
        }
    }

    #[test]
    fn manual_peer_comparison_is_window_and_generation_bound() {
        let (mut owner, mut verifier, wakes, _) = fixture();
        display(&mut verifier, &"a".repeat(64));
        owner.advance();
        let view = owner.view().unwrap();
        assert_eq!(view.channel, 7);
        assert_eq!(view.fingerprint, "a".repeat(64));
        assert!(matches!(verifier.poll_decision(), NativeDecision::Pending));
        assert_eq!(
            decide(
                &mut owner,
                WindowId::for_test(1),
                view.generation,
                Some(&"a".repeat(64))
            ),
            Err(Error::Unavailable)
        );
        assert_eq!(
            decide(
                &mut owner,
                view.window,
                view.generation,
                Some(&"g".repeat(64))
            ),
            Err(Error::Unavailable)
        );
        assert_eq!(
            decide(
                &mut owner,
                view.window,
                view.generation,
                Some(&"a".repeat(65))
            ),
            Err(Error::Unavailable)
        );
        assert!(view.is_current());
        decide(
            &mut owner,
            view.window,
            view.generation,
            Some(&"B".repeat(64)),
        )
        .unwrap();
        assert!(!view.is_current());
        assert!(wakes.load(Ordering::Relaxed) > 0);
        assert!(matches!(
            decision(&mut verifier),
            NativeDecision::ComparedPeer(value) if value == [b'b'; 64]
        ));
        assert_eq!(
            decide(
                &mut owner,
                view.window,
                view.generation,
                Some(&"a".repeat(64))
            ),
            Err(Error::Unavailable)
        );
    }

    #[test]
    fn retirement_invalidates_old_views_before_surface_reuse() {
        let (mut owner, mut verifier, _, stop) = fixture();
        display(&mut verifier, &"1".repeat(64));
        owner.advance();
        let old = owner.view().unwrap();
        verifier.begin_retirement().unwrap();
        assert!(!old.is_current());
        assert_eq!(
            decide(
                &mut owner,
                old.window,
                old.generation,
                Some(&"1".repeat(64))
            ),
            Err(Error::Unavailable)
        );
        let privacy = Arc::new(PrivacyGate::new(false));
        assert!(matches!(
            owner.surface().reserve(
                8,
                WindowId::PRIMARY,
                stop.clone(),
                privacy.clone(),
                Arc::new(|| {}),
                Arc::new(|| {})
            ),
            Err(Error::Busy)
        ));
        verifier.cancel().unwrap();
        owner.advance();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut next = loop {
            match owner.surface().reserve(
                8,
                WindowId::PRIMARY,
                stop.clone(),
                privacy.clone(),
                Arc::new(|| {}),
                Arc::new(|| {}),
            ) {
                Ok(verifier) => break verifier,
                Err(Error::Busy) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("could not reuse comparison surface: {error:?}"),
            }
        };
        display(&mut next, &"2".repeat(64));
        owner.advance();
        let current = owner.view().unwrap();
        assert_ne!(old.generation, current.generation);
        assert_eq!(
            decide(
                &mut owner,
                old.window,
                old.generation,
                Some(&"2".repeat(64))
            ),
            Err(Error::Unavailable)
        );
        assert!(current.is_current());
    }

    #[test]
    fn stop_invalidates_the_view_and_wakes_the_waiting_owner() {
        let (mut owner, mut verifier, wakes, stop) = fixture();
        display(&mut verifier, &"3".repeat(64));
        owner.advance();
        let view = owner.view().unwrap();
        super::super::set_admission_flag(&stop, true);
        assert!(!view.is_current());
        assert_eq!(
            decide(
                &mut owner,
                view.window,
                view.generation,
                Some(&"3".repeat(64))
            ),
            Err(Error::Unavailable)
        );
        owner.revoke();
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        assert!(matches!(decision(&mut verifier), NativeDecision::Denied));
    }

    #[test]
    fn privacy_cut_retires_the_prompt_and_wakes_the_waiter() {
        let (mut owner, mut verifier, wakes, _) = fixture();
        display(&mut verifier, &"7".repeat(64));
        owner.advance();
        let view = owner.view().unwrap();
        verifier.live.privacy.close();
        owner.advance();
        assert!(!view.is_current());
        assert!(owner.view().is_none());
        assert!(wakes.load(Ordering::Relaxed) > 0);
        assert!(matches!(decision(&mut verifier), NativeDecision::Denied));
    }

    #[test]
    fn display_and_poll_do_not_spin_on_the_admission_lock() {
        let (mut owner, mut verifier, wakes, _) = fixture();
        let guard = festerm_session::automation_admission_lock().unwrap();
        verifier.show_local_fingerprint(&"4".repeat(64)).unwrap();
        owner.advance();
        let view = owner.view().unwrap();
        assert!(matches!(verifier.poll_decision(), NativeDecision::Pending));
        assert_eq!(wakes.load(Ordering::Relaxed), 0);
        drop(guard);

        decide(
            &mut owner,
            view.window,
            view.generation,
            Some(&"4".repeat(64)),
        )
        .unwrap();
        let guard = festerm_session::automation_admission_lock().unwrap();
        assert!(matches!(
            verifier.poll_decision(),
            NativeDecision::ComparedPeer(value) if value == [b'4'; 64]
        ));
        drop(guard);
    }

    #[test]
    fn dropping_the_ui_owner_invalidates_and_wakes_the_waiter() {
        let (mut owner, mut verifier, wakes, _) = fixture();
        display(&mut verifier, &"5".repeat(64));
        owner.advance();
        let view = owner.view().unwrap();
        drop(owner);
        assert!(!view.is_current());
        assert!(wakes.load(Ordering::Relaxed) > 0);
        assert!(matches!(decision(&mut verifier), NativeDecision::Denied));
    }

    #[test]
    fn dropping_the_ui_owner_before_display_retires_the_reservation() {
        let (owner, mut verifier, wakes, stop) = fixture();
        let surface = owner.surface();
        let privacy = verifier.live.privacy.clone();
        drop(owner);

        assert!(wakes.load(Ordering::Relaxed) > 0);
        assert!(matches!(verifier.poll_decision(), NativeDecision::Denied));
        assert!(verifier.show_local_fingerprint(&"5".repeat(64)).is_err());
        assert!(matches!(
            surface.reserve(
                8,
                WindowId::PRIMARY,
                stop,
                privacy,
                Arc::new(|| {}),
                Arc::new(|| {})
            ),
            Err(Error::Unavailable)
        ));
    }

    #[test]
    fn stale_reservation_is_bounded_and_reclaimed_before_reuse() {
        let (mut owner, mut verifier, _, stop) = fixture();
        let privacy = verifier.live.privacy.clone();
        let surface = owner.surface();
        verifier.cancel().unwrap();
        assert!(matches!(
            surface.reserve(
                8,
                WindowId::PRIMARY,
                stop.clone(),
                privacy.clone(),
                Arc::new(|| {}),
                Arc::new(|| {})
            ),
            Err(Error::Busy)
        ));
        owner.advance();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut next = loop {
            match surface.reserve(
                8,
                WindowId::PRIMARY,
                stop.clone(),
                privacy.clone(),
                Arc::new(|| {}),
                Arc::new(|| {}),
            ) {
                Ok(verifier) => break verifier,
                Err(Error::Busy) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("could not reuse comparison surface: {error:?}"),
            }
        };
        display(&mut next, &"9".repeat(64));
        owner.advance();
        let view = owner.view().unwrap();
        assert_eq!(view.channel, 8);
        assert_ne!(view.generation, verifier.generation);
    }

    #[test]
    fn queued_stale_prompt_is_reclaimed_before_surface_reuse() {
        let (mut owner, mut verifier, _, stop) = fixture();
        let privacy = verifier.live.privacy.clone();
        let surface = owner.surface();
        owner.advance();
        display(&mut verifier, &"a".repeat(64));
        verifier.cancel().unwrap();
        assert!(matches!(
            surface.reserve(
                8,
                WindowId::PRIMARY,
                stop.clone(),
                privacy.clone(),
                Arc::new(|| {}),
                Arc::new(|| {})
            ),
            Err(Error::Busy)
        ));
        owner.advance();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut next = loop {
            match surface.reserve(
                8,
                WindowId::PRIMARY,
                stop.clone(),
                privacy.clone(),
                Arc::new(|| {}),
                Arc::new(|| {}),
            ) {
                Ok(verifier) => break verifier,
                Err(Error::Busy) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("could not reuse comparison surface: {error:?}"),
            }
        };
        display(&mut next, &"b".repeat(64));
        owner.advance();
        assert_eq!(owner.view().unwrap().fingerprint, "b".repeat(64));
    }

    #[test]
    fn reservation_replacement_during_advance_reclaims_old_state() {
        let (mut owner, mut verifier, _, stop) = fixture();
        let privacy = verifier.live.privacy.clone();
        let surface = owner.surface();
        display(&mut verifier, &"a".repeat(64));
        owner.advance();
        let (entered, entered_rx) = mpsc::sync_channel(1);
        let (resume, resume_rx) = mpsc::sync_channel(1);
        ADVANCE_TEST_GATE.with(|slot| {
            *slot.borrow_mut() = Some(AdvanceTestGate {
                entered,
                resume: resume_rx,
            });
        });
        let worker = thread::spawn(move || {
            entered_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("comparison advance did not start");
            verifier.cancel().unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut next = loop {
                match surface.reserve(
                    8,
                    WindowId::PRIMARY,
                    stop.clone(),
                    privacy.clone(),
                    Arc::new(|| {}),
                    Arc::new(|| {}),
                ) {
                    Ok(verifier) => break verifier,
                    Err(Error::Busy) if Instant::now() < deadline => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("could not replace comparison: {error:?}"),
                }
            };
            display(&mut next, &"b".repeat(64));
            resume.send(()).unwrap();
            next
        });
        owner.advance();
        let mut next = worker.join().unwrap();
        assert!(!owner.quarantined());
        assert_eq!(owner.view().unwrap().fingerprint, "b".repeat(64));
        assert!(matches!(next.poll_decision(), NativeDecision::Pending));
    }

    #[test]
    fn rejection_wakes_and_denies_the_current_comparison() {
        let (mut owner, mut verifier, wakes, _) = fixture();
        display(&mut verifier, &"6".repeat(64));
        owner.advance();
        let view = owner.view().unwrap();
        decide(&mut owner, view.window, view.generation, None).unwrap();
        assert!(!view.is_current());
        assert!(wakes.load(Ordering::Relaxed) > 0);
        assert!(matches!(decision(&mut verifier), NativeDecision::Denied));
    }

    #[test]
    fn same_frame_cancellation_cannot_confirm_a_direct_comparison() {
        use super::super::{Coordinator, NativeCommand, NativeWindowCommand};

        for conflict in [false, true] {
            let (owner, mut verifier, _, stop) = fixture();
            let mut coordinator = Coordinator::default();
            coordinator.stop = stop.clone();
            coordinator.privacy_gate = verifier.live.privacy.clone();
            coordinator.verification = owner;
            display(&mut verifier, &"8".repeat(64));
            coordinator.verification.advance();
            let view = coordinator.verification.view().unwrap();
            let cancellation = if conflict {
                NativeCommand::DenyDirectPeer {
                    generation: view.generation,
                }
            } else {
                NativeCommand::ClearWorkflow
            };
            let commands = vec![
                NativeWindowCommand {
                    window: view.window,
                    command: NativeCommand::CompareDirectPeer {
                        generation: view.generation,
                        fingerprint: "8".repeat(64),
                    },
                },
                NativeWindowCommand {
                    window: view.window,
                    command: cancellation,
                },
            ];
            assert!(coordinator.commands(commands, &eframe::egui::Context::default(), &[]));
            if conflict {
                assert!(stop.load(Ordering::Acquire));
                assert!(!view.is_current());
                assert!(matches!(decision(&mut verifier), NativeDecision::Denied));
            } else {
                assert!(!stop.load(Ordering::Acquire));
                assert!(view.is_current());
                assert!(matches!(verifier.poll_decision(), NativeDecision::Pending));
            }
        }
    }

    #[test]
    fn failed_cleanup_quarantines_the_surface() {
        let (owner, mut verifier, _, stop) = fixture();
        verifier.wake_ui = Arc::new(|| panic!("injected comparison wake failure"));
        assert!(verifier.begin_retirement().is_err());
        assert!(verifier.cancel().is_err());
        assert!(owner.quarantined());
        assert!(matches!(
            owner.surface().reserve(
                9,
                WindowId::PRIMARY,
                stop,
                Arc::new(PrivacyGate::new(false)),
                Arc::new(|| {}),
                Arc::new(|| {})
            ),
            Err(Error::Unavailable)
        ));
    }
}
