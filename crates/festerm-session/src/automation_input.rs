use std::{
    fmt,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use crate::{
    try_automation_admission_lock, AutomationAdmissionGuard, AutomationAdmissionTryError,
    AutomationEpoch, SessionOperation, SessionSendError, MAX_IO_CHUNK_BYTES,
};

const RESERVATION_LIFETIME: Duration = Duration::from_millis(250);
const PENDING: u8 = 0;
const COMMITTING: u8 = 1;
const CLAIMED: u8 = 2;
const READY: u8 = 3;
const CANCELLED: u8 = 4;
const TAKEN: u8 = 5;

struct Slot {
    bytes: Arc<[u8]>,
    deadline: Instant,
    state: AtomicU8,
    controller_instance: u64,
    attachment: Arc<AutomationEpoch>,
    attachment_generation: u64,
}

impl Slot {
    fn cancel_open(&self) {
        let mut state = self.state.load(Ordering::Acquire);
        while state == PENDING || state == COMMITTING || state == CLAIMED {
            match self.state.compare_exchange_weak(
                state,
                CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => state = actual,
            }
        }
    }
}

/// A reserved backend queue position, not permission to control a session.
///
/// The application must keep its worker-admission and privacy/lifecycle fences
/// held through `commit_with` returning, check this reservation's controller,
/// backend identity and attachment generation against the exact target, and
/// recheck the lease, broker authority and immutable bytes inside the callback.
/// The callback must perform only bounded broker admission, not terminal I/O.
pub struct ReservedAutomationInput {
    slot: Arc<Slot>,
}

/// The backend's ordered half of an automation input reservation.
pub struct PendingAutomationInput {
    slot: Arc<Slot>,
}

/// The backend may send bytes only after observing `Ready`.
pub enum AutomationInputStatus {
    Pending,
    Ready(Arc<[u8]>),
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReserveAutomationInputError {
    Unsupported,
    Empty,
    Busy,
    Unavailable,
    Rejected(SessionSendError),
}

impl fmt::Display for ReserveAutomationInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => formatter.write_str("automation input reservation is unsupported"),
            Self::Empty => formatter.write_str("automation input must not be empty"),
            Self::Busy => formatter.write_str("automation admission is busy"),
            Self::Unavailable => formatter.write_str("automation attachment is unavailable"),
            Self::Rejected(error) => {
                write!(formatter, "automation input reservation rejected: {error}")
            }
        }
    }
}

impl std::error::Error for ReserveAutomationInputError {}

/// A failed reservation commit. `LostAfterCommit` requires conservative
/// unknown-outcome handling because the authority callback already succeeded.
#[derive(Debug, Eq, PartialEq)]
pub enum CommitAutomationInputError<E> {
    ClosedOrExpired,
    Refused(E),
    LostAfterCommit,
}

/// Creates the two halves of one bounded, immutable reservation.
///
/// The backend supplies its own attachment identity, not one selected by the
/// caller, and the host supplies its controller identity. The attachment's
/// running generation is captured nonblockingly at creation. A backend must
/// enqueue the pending half in its ordinary ordered input queue before
/// returning the reserved half. Dropping either half before commitment cancels
/// it. A backend must poll the pending half and may not allow later input to
/// overtake it.
pub fn automation_input_slot(
    bytes: &[u8],
    controller_instance: u64,
    attachment: Arc<AutomationEpoch>,
) -> Result<(ReservedAutomationInput, PendingAutomationInput), ReserveAutomationInputError> {
    if bytes.is_empty() {
        return Err(ReserveAutomationInputError::Empty);
    }
    if bytes.len() > MAX_IO_CHUNK_BYTES {
        return Err(ReserveAutomationInputError::Rejected(
            SessionSendError::TooLarge {
                operation: SessionOperation::Input,
                maximum: MAX_IO_CHUNK_BYTES,
                actual: bytes.len(),
            },
        ));
    }
    if controller_instance == 0 {
        return Err(ReserveAutomationInputError::Unavailable);
    }
    let guard = try_automation_admission_lock().map_err(|error| match error {
        AutomationAdmissionTryError::Busy => ReserveAutomationInputError::Busy,
        AutomationAdmissionTryError::Poisoned => ReserveAutomationInputError::Unavailable,
    })?;
    let attachment_generation = attachment
        .capture_under_admission(&guard)
        .ok_or(ReserveAutomationInputError::Unavailable)?;
    Ok(slot_with_deadline(
        bytes,
        Instant::now() + RESERVATION_LIFETIME,
        controller_instance,
        attachment,
        attachment_generation,
    ))
}

fn slot_with_deadline(
    bytes: &[u8],
    deadline: Instant,
    controller_instance: u64,
    attachment: Arc<AutomationEpoch>,
    attachment_generation: u64,
) -> (ReservedAutomationInput, PendingAutomationInput) {
    let slot = Arc::new(Slot {
        bytes: Arc::from(bytes),
        deadline,
        state: AtomicU8::new(PENDING),
        controller_instance,
        attachment,
        attachment_generation,
    });
    (
        ReservedAutomationInput { slot: slot.clone() },
        PendingAutomationInput { slot },
    )
}

impl ReservedAutomationInput {
    /// Checks the backend-owned attachment and host controller at the same
    /// admission boundary that must remain held through `commit_with`.
    pub fn matches_attachment(
        &self,
        controller_instance: u64,
        attachment: &Arc<AutomationEpoch>,
        generation: u64,
        _guard: &AutomationAdmissionGuard,
    ) -> bool {
        self.slot.controller_instance == controller_instance
            && Arc::ptr_eq(&self.slot.attachment, attachment)
            && self.slot.attachment_generation == generation
            && self.slot.attachment.is_current(generation)
    }

    /// Commits the exact immutable bytes after the caller's authority check.
    ///
    /// The callback is called at most once and must recheck the complete
    /// authority state. The caller must retain all admission fences until this
    /// method returns, including while the bytes are published to the backend.
    /// If the receiver disappears or its deadline expires before the
    /// admission claim is validated, the bytes will not become ready, and
    /// `LostAfterCommit` reports that the callback nevertheless succeeded.
    pub fn commit_with<T, E>(
        self,
        commit: impl FnOnce(&[u8]) -> Result<T, E>,
    ) -> Result<T, CommitAutomationInputError<E>> {
        self.commit_using(Instant::now, commit)
    }

    fn commit_using<T, E>(
        self,
        now: impl Fn() -> Instant,
        commit: impl FnOnce(&[u8]) -> Result<T, E>,
    ) -> Result<T, CommitAutomationInputError<E>> {
        if now() >= self.slot.deadline {
            self.slot.cancel_open();
            return Err(CommitAutomationInputError::ClosedOrExpired);
        }
        if self
            .slot
            .state
            .compare_exchange(PENDING, COMMITTING, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(CommitAutomationInputError::ClosedOrExpired);
        }
        // Check after taking the slot so descheduling cannot start a late
        // authority callback without first proving the claim was timely.
        if now() >= self.slot.deadline {
            self.slot.cancel_open();
            return Err(CommitAutomationInputError::ClosedOrExpired);
        }
        let outcome = commit(&self.slot.bytes);
        match outcome {
            Ok(value) => {
                if now() >= self.slot.deadline {
                    self.slot.cancel_open();
                    return Err(CommitAutomationInputError::LostAfterCommit);
                }
                if self
                    .slot
                    .state
                    .compare_exchange(COMMITTING, CLAIMED, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    return Err(CommitAutomationInputError::LostAfterCommit);
                }
                // A clock read after the claim proves the admission transition
                // occurred before expiry even if this thread was descheduled.
                if now() >= self.slot.deadline {
                    self.slot.cancel_open();
                    return Err(CommitAutomationInputError::LostAfterCommit);
                }
                self.slot
                    .state
                    .compare_exchange(CLAIMED, READY, Ordering::AcqRel, Ordering::Acquire)
                    .map(|_| value)
                    .map_err(|_| CommitAutomationInputError::LostAfterCommit)
            }
            Err(error) => {
                self.slot.cancel_open();
                Err(CommitAutomationInputError::Refused(error))
            }
        }
    }
}

impl Drop for ReservedAutomationInput {
    fn drop(&mut self) {
        self.slot.cancel_open();
    }
}

impl PendingAutomationInput {
    /// Expires an unresolved slot without consuming an already committed one.
    pub fn is_cancelled(&self) -> bool {
        if Instant::now() >= self.slot.deadline {
            self.slot.cancel_open();
        }
        matches!(self.slot.state.load(Ordering::Acquire), CANCELLED | TAKEN)
    }

    /// Whether the attachment generation bound at reservation is still live.
    /// A backend that can replace its attachment must check this under the
    /// admission boundary before writing ready bytes.
    pub fn attachment_is_current(&self, guard: &AutomationAdmissionGuard) -> bool {
        self.slot
            .attachment
            .is_current_under_admission(guard, self.slot.attachment_generation)
    }

    pub fn poll(&mut self) -> AutomationInputStatus {
        self.poll_at(Instant::now())
    }

    fn poll_at(&mut self, now: Instant) -> AutomationInputStatus {
        if now >= self.slot.deadline {
            self.slot.cancel_open();
        }
        loop {
            match self.slot.state.load(Ordering::Acquire) {
                PENDING | COMMITTING | CLAIMED => return AutomationInputStatus::Pending,
                READY => {
                    if self
                        .slot
                        .state
                        .compare_exchange(READY, TAKEN, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        return AutomationInputStatus::Ready(self.slot.bytes.clone());
                    }
                }
                _ => return AutomationInputStatus::Cancelled,
            }
        }
    }
}

impl Drop for PendingAutomationInput {
    fn drop(&mut self) {
        self.slot.cancel_open();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn running_attachment() -> Arc<AutomationEpoch> {
        let attachment = Arc::new(AutomationEpoch::new());
        attachment.transition(true);
        attachment
    }

    fn reserve(bytes: &[u8]) -> (ReservedAutomationInput, PendingAutomationInput) {
        reserve_for(bytes, 1, running_attachment()).unwrap()
    }

    fn reserve_for(
        bytes: &[u8],
        controller_instance: u64,
        attachment: Arc<AutomationEpoch>,
    ) -> Result<(ReservedAutomationInput, PendingAutomationInput), ReserveAutomationInputError>
    {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match automation_input_slot(bytes, controller_instance, attachment.clone()) {
                Err(ReserveAutomationInputError::Busy) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                result => return result,
            }
        }
    }

    fn slot_with_deadline(
        bytes: &[u8],
        deadline: Instant,
    ) -> (ReservedAutomationInput, PendingAutomationInput) {
        let attachment = running_attachment();
        let generation = attachment.capture().unwrap();
        super::slot_with_deadline(bytes, deadline, 1, attachment, generation)
    }

    #[test]
    fn reserved_input_waits_for_exact_commit_and_is_delivered_once() {
        let (reservation, mut pending) = reserve(b"secret input");
        assert!(matches!(pending.poll(), AutomationInputStatus::Pending));
        reservation
            .commit_with(|bytes| {
                assert_eq!(bytes, b"secret input");
                Ok::<_, ()>(())
            })
            .unwrap();
        assert!(
            matches!(pending.poll(), AutomationInputStatus::Ready(bytes) if &*bytes == b"secret input")
        );
        assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
    }

    #[test]
    fn cancellation_probe_preserves_a_timely_committed_slot() {
        let (reservation, mut pending) =
            slot_with_deadline(b"committed", Instant::now() + Duration::from_secs(1));
        reservation.commit_with(|_| Ok::<_, ()>(())).unwrap();
        Arc::get_mut(&mut pending.slot).unwrap().deadline = Instant::now() - Duration::from_secs(1);
        assert!(!pending.is_cancelled());
        assert!(
            matches!(pending.poll(), AutomationInputStatus::Ready(bytes) if &*bytes == b"committed")
        );
        assert!(pending.is_cancelled());

        let (expired, pending) =
            slot_with_deadline(b"expired", Instant::now() - Duration::from_secs(1));
        assert!(pending.is_cancelled());
        assert!(matches!(
            expired.commit_with(|_| Ok::<_, ()>(())),
            Err(CommitAutomationInputError::ClosedOrExpired)
        ));
    }

    #[test]
    fn pending_input_detects_a_replaced_attachment() {
        let attachment = running_attachment();
        let (_reservation, pending) = reserve_for(b"bound", 1, attachment.clone()).unwrap();
        let guard = loop {
            if let Ok(guard) = try_automation_admission_lock() {
                break guard;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        assert!(pending.attachment_is_current(&guard));
        drop(guard);
        attachment.transition(false);
        attachment.transition(true);
        let guard = crate::automation_admission_lock().unwrap();
        assert!(!pending.attachment_is_current(&guard));
    }

    #[test]
    fn reservation_refusal_drop_and_missing_receiver_release_no_input() {
        let (reservation, mut pending) = reserve(b"refused");
        assert_eq!(
            reservation.commit_with(|_| Err::<(), _>("refused")),
            Err(CommitAutomationInputError::Refused("refused"))
        );
        assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));

        let (reservation, mut pending) = reserve(b"dropped");
        drop(reservation);
        assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));

        let (reservation, mut pending) = reserve(b"panic");
        let result = std::panic::catch_unwind(|| {
            let _ = reservation.commit_with::<(), ()>(|_| panic!("commit failed"));
        });
        assert!(result.is_err());
        assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));

        let (reservation, pending) = reserve(b"lost");
        drop(pending);
        assert_eq!(
            reservation.commit_with(|_| panic!("closed slot must not authorize")),
            Err::<(), CommitAutomationInputError<()>>(CommitAutomationInputError::ClosedOrExpired)
        );
    }

    #[test]
    fn reservation_expires_while_commit_is_stalled_without_releasing_input() {
        let deadline = Instant::now() + Duration::from_secs(30);
        let (reservation, mut pending) = slot_with_deadline(b"stalled", deadline);
        let (entered_send, entered) = mpsc::sync_channel(1);
        let (resume, resumed) = mpsc::sync_channel(1);
        let commit = std::thread::spawn(move || {
            reservation.commit_with(|_| {
                entered_send.send(()).unwrap();
                resumed.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok::<_, ()>(())
            })
        });
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(
            pending.poll_at(deadline),
            AutomationInputStatus::Cancelled
        ));
        resume.send(()).unwrap();
        assert_eq!(
            commit.join().unwrap(),
            Err(CommitAutomationInputError::LostAfterCommit)
        );
        assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
    }

    #[test]
    fn receiver_loss_during_commit_reports_a_lost_authority_boundary() {
        let (reservation, pending) =
            slot_with_deadline(b"receiver lost", Instant::now() + Duration::from_secs(30));
        let (entered_send, entered) = mpsc::sync_channel(1);
        let (resume, resumed) = mpsc::sync_channel(1);
        let commit = std::thread::spawn(move || {
            reservation.commit_with(|_| {
                entered_send.send(()).unwrap();
                resumed.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok::<_, ()>(())
            })
        });
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(pending);
        resume.send(()).unwrap();
        assert_eq!(
            commit.join().unwrap(),
            Err(CommitAutomationInputError::LostAfterCommit)
        );
    }

    #[test]
    fn reservation_refuses_late_commit_even_if_backend_has_not_polled() {
        let start = Instant::now();
        let deadline = start + Duration::from_secs(30);
        let (reservation, mut pending) = slot_with_deadline(b"late", deadline);
        let clock = std::cell::Cell::new(start);
        let result = reservation.commit_using(
            || clock.get(),
            |_| {
                clock.set(deadline);
                Ok::<_, ()>(())
            },
        );
        assert_eq!(result, Err(CommitAutomationInputError::LostAfterCommit));
        assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
    }

    #[test]
    fn reservation_refuses_a_claim_that_crossed_the_deadline() {
        let start = Instant::now();
        let deadline = start + Duration::from_secs(30);
        let (reservation, mut pending) = slot_with_deadline(b"late claim", deadline);
        let reads = std::cell::Cell::new(0);
        let result = reservation.commit_using(
            || {
                reads.set(reads.get() + 1);
                if reads.get() < 4 {
                    start
                } else {
                    deadline
                }
            },
            |_| Ok::<_, ()>(()),
        );
        assert_eq!(result, Err(CommitAutomationInputError::LostAfterCommit));
        assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
    }

    #[test]
    fn reservation_refuses_a_late_start_without_invoking_authority() {
        let start = Instant::now();
        let deadline = start + Duration::from_secs(30);
        let (reservation, mut pending) = slot_with_deadline(b"late start", deadline);
        let reads = std::cell::Cell::new(0);
        let result = reservation.commit_using(
            || {
                reads.set(reads.get() + 1);
                if reads.get() == 1 {
                    start
                } else {
                    deadline
                }
            },
            |_| panic!("late reservation must not invoke authority"),
        );
        assert_eq!(
            result,
            Err::<(), CommitAutomationInputError<()>>(CommitAutomationInputError::ClosedOrExpired)
        );
        assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
    }

    #[test]
    fn receiver_loss_during_claim_prevents_publication() {
        let start = Instant::now();
        let deadline = start + Duration::from_secs(30);
        let (reservation, pending) = slot_with_deadline(b"lost claim", deadline);
        let pending = std::cell::RefCell::new(Some(pending));
        let reads = std::cell::Cell::new(0);
        let result = reservation.commit_using(
            || {
                reads.set(reads.get() + 1);
                if reads.get() == 4 {
                    drop(pending.borrow_mut().take());
                }
                start
            },
            |_| Ok::<_, ()>(()),
        );
        assert_eq!(result, Err(CommitAutomationInputError::LostAfterCommit));
    }

    #[test]
    fn timely_admission_remains_ready_after_the_backend_deadline() {
        let start = Instant::now();
        let deadline = start + Duration::from_secs(30);
        let (reservation, mut pending) = slot_with_deadline(b"admitted", deadline);
        reservation
            .commit_using(|| start, |_| Ok::<_, ()>(()))
            .unwrap();
        assert!(
            matches!(pending.poll_at(deadline), AutomationInputStatus::Ready(bytes) if &*bytes == b"admitted")
        );
    }

    #[test]
    fn reservation_enforces_input_bounds_and_expiry_before_authority() {
        let attachment = running_attachment();
        assert!(matches!(
            automation_input_slot(b"", 1, attachment.clone()),
            Err(ReserveAutomationInputError::Empty)
        ));
        assert!(matches!(
            automation_input_slot(&vec![0; MAX_IO_CHUNK_BYTES + 1], 1, attachment),
            Err(ReserveAutomationInputError::Rejected(
                SessionSendError::TooLarge { .. }
            ))
        ));
        let (reservation, mut pending) = slot_with_deadline(b"expired", Instant::now());
        assert_eq!(
            reservation.commit_with(|_| panic!("expired slot must not authorize")),
            Err::<(), CommitAutomationInputError<()>>(CommitAutomationInputError::ClosedOrExpired)
        );
        assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
    }

    #[test]
    fn reservation_is_bound_to_controller_attachment_and_creation_generation() {
        let attachment = running_attachment();
        let other = running_attachment();
        let generation = attachment.capture().unwrap();
        let (reservation, mut pending) = reserve_for(b"same bytes", 7, attachment.clone()).unwrap();
        {
            let guard = crate::automation_admission_lock().unwrap();
            assert!(reservation.matches_attachment(7, &attachment, generation, &guard));
            assert!(!reservation.matches_attachment(8, &attachment, generation, &guard));
            assert!(!reservation.matches_attachment(7, &other, generation, &guard));
        }
        attachment.transition(false);
        attachment.transition(true);
        let replacement_generation = attachment.capture().unwrap();
        {
            let guard = crate::automation_admission_lock().unwrap();
            assert!(!reservation.matches_attachment(7, &attachment, generation, &guard));
            assert!(!reservation.matches_attachment(
                7,
                &attachment,
                replacement_generation,
                &guard
            ));
        }
        drop(reservation);
        assert!(matches!(pending.poll(), AutomationInputStatus::Cancelled));
    }

    #[test]
    fn reservation_capture_refuses_busy_or_unavailable_attachment() {
        let attachment = running_attachment();
        let guard = crate::automation_admission_lock().unwrap();
        assert!(matches!(
            automation_input_slot(b"input", 1, attachment.clone()),
            Err(ReserveAutomationInputError::Busy)
        ));
        drop(guard);
        attachment.transition(false);
        assert!(matches!(
            reserve_for(b"input", 1, attachment),
            Err(ReserveAutomationInputError::Unavailable)
        ));
    }
}
