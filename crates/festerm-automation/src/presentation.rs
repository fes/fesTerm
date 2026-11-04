use super::*;
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::time::Duration;

/// One outstanding attempt is allowed for the entire application.
pub const MAX_PRESENTATION_WAIT: Duration = Duration::from_secs(5);
pub const MAX_PRESENTATION_ATTEMPTS_PER_GRANT: u32 = 16;
const MAX_PRESENTATION_ATTEMPTS_PER_BROKER: u32 = 4096;

/// A stable application-owned window identity, not a viewport index or wire ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentationWindowId(u64);

impl PresentationWindowId {
    /// Only the trusted application may map its own non-reused window ID here.
    pub const fn from_host(id: u64) -> Self {
        Self(id)
    }
}

/// A frozen native review of presentation authority for one enrollment.
#[derive(Debug)]
pub struct PresentationApprovalProposal {
    connection: ConnectionId,
    registration: ControllerRegistrationId,
    enrollment: ControllerEnrollmentId,
    target: TargetRef,
    review: u64,
    expires: Instant,
}

impl PresentationApprovalProposal {
    pub fn registration(&self) -> ControllerRegistrationId {
        self.registration
    }

    pub fn enrollment(&self) -> ControllerEnrollmentId {
        self.enrollment
    }

    pub fn target(&self) -> TargetRef {
        self.target
    }
}

/// Separate native consent to present the exact enrolled target. This is not
/// an enrollment decision, an observation grant, or an input handoff.
///
/// ```compile_fail
/// use festerm_automation::NativeControllerPresentationApproval;
/// let _ = serde_json::from_str::<NativeControllerPresentationApproval>("{}");
/// ```
pub struct NativeControllerPresentationApproval {
    proposal: PresentationApprovalProposal,
    lifetime: Duration,
}

impl NativeControllerPresentationApproval {
    pub fn new(proposal: PresentationApprovalProposal, lifetime: Duration) -> Self {
        Self { proposal, lifetime }
    }
}

/// An opaque host-owned freshness revision, not a credential or grant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentationRevision {
    instance: [u8; 16],
    sequence: u64,
}

impl Serialize for PresentationRevision {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut bytes = [0_u8; 24];
        bytes[..16].copy_from_slice(&self.instance);
        bytes[16..].copy_from_slice(&self.sequence.to_be_bytes());
        let mut encoded = String::with_capacity(48);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in bytes {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        serializer.serialize_str(&encoded)
    }
}

impl<'de> Deserialize<'de> for PresentationRevision {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct RevisionVisitor;
        impl de::Visitor<'_> for RevisionVisitor {
            type Value = PresentationRevision;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a 48-character lowercase opaque presentation revision")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Self::Value, E> {
                if value.len() != 48 {
                    return Err(E::invalid_value(de::Unexpected::Str(value), &self));
                }
                let mut bytes = [0_u8; 24];
                for (pair, output) in value.as_bytes().as_chunks::<2>().0.iter().zip(&mut bytes) {
                    let digit = |byte: u8| match byte {
                        b'0'..=b'9' => Some(byte - b'0'),
                        b'a'..=b'f' => Some(byte - b'a' + 10),
                        _ => None,
                    };
                    let Some(high) = digit(pair[0]) else {
                        return Err(E::invalid_value(de::Unexpected::Str(value), &self));
                    };
                    let Some(low) = digit(pair[1]) else {
                        return Err(E::invalid_value(de::Unexpected::Str(value), &self));
                    };
                    *output = (high << 4) | low;
                }
                let mut instance = [0_u8; 16];
                instance.copy_from_slice(&bytes[..16]);
                let mut sequence = [0_u8; 8];
                sequence.copy_from_slice(&bytes[16..]);
                Ok(PresentationRevision {
                    instance,
                    sequence: u64::from_be_bytes(sequence),
                })
            }
        }
        deserializer.deserialize_str(RevisionVisitor)
    }
}

/// Success identifies the exact enrolled attachment. The revision is bound
/// internally to its selected tab, window and qualified native activation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmedPresentation {
    pub target: TargetRef,
    pub revision: PresentationRevision,
}

/// Frozen by trusted application code in a control request. The client-supplied
/// revision is meaningful only with this connection's exact enrollment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentationRequirement {
    registration: ControllerRegistrationId,
    enrollment: ControllerEnrollmentId,
    revision: PresentationRevision,
}

impl PresentationRequirement {
    pub fn new(
        registration: ControllerRegistrationId,
        enrollment: ControllerEnrollmentId,
        revision: PresentationRevision,
    ) -> Self {
        Self {
            registration,
            enrollment,
            revision,
        }
    }

    pub fn revision(self) -> PresentationRevision {
        self.revision
    }
}

/// A trusted, non-wire identity for one uninterrupted native event source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentationMonitorId {
    instance: [u8; 16],
    sequence: u64,
}

/// Actual state obtained by a *qualified* native source and freshness barrier.
/// A cached egui focus flag, focus command, PID or client claim is not such a
/// source. `Unknown` permanently stops this monitor until explicitly replaced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativePresentationState {
    Foreground {
        window: PresentationWindowId,
        selected: Option<TargetRef>,
        /// Application-owned monotonic revision of all tab/window changes.
        workspace_revision: u64,
    },
    Inactive,
    Unknown,
    ForegroundRefused,
}

/// Non-serializable evidence produced after the native barrier. The caller must
/// hold the host admission boundary until confirmation, disclosure, or input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativePresentationObservation {
    monitor: PresentationMonitorId,
    transitions: u64,
    sample: u64,
    window: PresentationWindowId,
    selected: TargetRef,
    workspace_revision: u64,
}

/// Host-only handle to one bounded, cancellable activation attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentationAttempt {
    connection: ConnectionId,
    id: u64,
    target: TargetRef,
    window: PresentationWindowId,
    deadline: Instant,
}

impl PresentationAttempt {
    pub fn target(self) -> TargetRef {
        self.target
    }

    pub fn window(self) -> PresentationWindowId {
        self.window
    }

    pub fn deadline(self) -> Instant {
        self.deadline
    }
}

#[derive(Clone, Copy)]
pub(super) struct PresentationGrant {
    enrollment: ControllerEnrollmentId,
    target: TargetRef,
    epoch: u64,
    expires: Instant,
    attempts: u32,
}

#[derive(Clone, Copy)]
struct Monitor {
    id: PresentationMonitorId,
    transitions: u64,
    samples: u64,
}

#[derive(Clone, Copy)]
enum Phase {
    Prepared,
    Waiting {
        after_sample: u64,
    },
    Confirmed {
        revision: PresentationRevision,
        transitions: u64,
        workspace_revision: u64,
        last_sample: u64,
    },
}

#[derive(Clone, Copy)]
struct Slot {
    attempt: PresentationAttempt,
    registration: ControllerRegistrationId,
    enrollment: ControllerEnrollmentId,
    grant_epoch: u64,
    grant_expires: Instant,
    monitor: PresentationMonitorId,
    phase: Phase,
}

#[derive(Default)]
pub(super) struct PresentationState {
    monitor: Option<Monitor>,
    slot: Option<Slot>,
    attempts: u32,
}

impl Broker {
    /// Initiated only by a native user action, never by a wire request. Starting
    /// a newer review withdraws the old presentation grant and supersedes older
    /// reviews, even if the supplied enrollment is already stale.
    pub fn prepare_controller_presentation_approval(
        &mut self,
        connection: ConnectionId,
        registration: ControllerRegistrationId,
        enrollment: ControllerEnrollmentId,
        now: Instant,
    ) -> Result<PresentationApprovalProposal> {
        self.poll_expiry(now);
        self.principals
            .get(&connection)
            .and_then(|principal| principal.controller_registrations.get(&registration))
            .ok_or(Error::Unauthorized)?;
        let review = self.mint()?;
        let record = self
            .principals
            .get_mut(&connection)
            .and_then(|principal| principal.controller_registrations.get_mut(&registration))
            .ok_or(Error::Unauthorized)?;
        record.presentation_review = Some(review);
        record.presentation_grant = None;
        self.cancel_presentation_for_registration(connection, registration);
        let target = self.resolve_controller_enrollment(connection, registration, enrollment)?;
        let expires = now
            .checked_add(MAX_GRANT_LIFETIME)
            .ok_or(Error::InvalidRequest)?;
        Ok(PresentationApprovalProposal {
            connection,
            registration,
            enrollment,
            target,
            review,
            expires,
        })
    }

    /// Accept only the reviewed, exact connection/enrollment/target. This does
    /// not authorize any other operation and never requests a native prompt.
    pub fn approve_controller_presentation(
        &mut self,
        connection: ConnectionId,
        decision: NativeControllerPresentationApproval,
        now: Instant,
    ) -> Result<()> {
        self.poll_expiry(now);
        let proposal = decision.proposal;
        if proposal.connection != connection {
            return Err(Error::Unauthorized);
        }
        let record = self
            .principals
            .get(&connection)
            .and_then(|principal| {
                principal
                    .controller_registrations
                    .get(&proposal.registration)
            })
            .ok_or(Error::Unauthorized)?;
        if record.presentation_review != Some(proposal.review) {
            return Err(Error::BindingMismatch);
        }
        if now >= proposal.expires {
            return Err(Error::Expired);
        }
        if decision.lifetime.is_zero() || decision.lifetime > MAX_GRANT_LIFETIME {
            return Err(Error::InvalidRequest);
        }
        if self.resolve_controller_enrollment(
            connection,
            proposal.registration,
            proposal.enrollment,
        )? != proposal.target
        {
            return Err(Error::BindingMismatch);
        }
        let expires = now
            .checked_add(decision.lifetime)
            .ok_or(Error::InvalidRequest)?;
        let epoch = self.mint()?;
        let record = self
            .principals
            .get_mut(&connection)
            .and_then(|principal| {
                principal
                    .controller_registrations
                    .get_mut(&proposal.registration)
            })
            .ok_or(Error::Unauthorized)?;
        record.presentation_review = None;
        record.presentation_grant = Some(PresentationGrant {
            enrollment: proposal.enrollment,
            target: proposal.target,
            epoch,
            expires,
            attempts: 0,
        });
        Ok(())
    }

    /// Withdraw this registration's pending review, grant and evidence without
    /// changing its enrollment or the connection's other authorities.
    pub fn revoke_controller_presentation(
        &mut self,
        connection: ConnectionId,
        registration: ControllerRegistrationId,
    ) -> Result<()> {
        let record = self
            .principals
            .get_mut(&connection)
            .and_then(|principal| principal.controller_registrations.get_mut(&registration))
            .ok_or(Error::Unauthorized)?;
        record.presentation_review = None;
        record.presentation_grant = None;
        self.cancel_presentation_for_registration(connection, registration);
        Ok(())
    }

    fn presentation_grant(
        &self,
        connection: ConnectionId,
        registration: ControllerRegistrationId,
        enrollment: ControllerEnrollmentId,
        now: Instant,
    ) -> Result<PresentationGrant> {
        let target = self.resolve_controller_enrollment(connection, registration, enrollment)?;
        let grant = self
            .principals
            .get(&connection)
            .and_then(|principal| principal.controller_registrations.get(&registration))
            .and_then(|record| record.presentation_grant)
            .filter(|grant| grant.enrollment == enrollment && grant.target == target)
            .ok_or(Error::Revoked)?;
        if now >= grant.expires {
            return Err(Error::Expired);
        }
        Ok(grant)
    }

    /// Installs a new event source only after the native integration has proved
    /// complete transition delivery and a freshness/admission ordering barrier.
    /// This API does not qualify a platform. Reinstallation invalidates every
    /// old attempt and revision; an unqualified application must never call it.
    pub fn start_qualified_presentation_monitor(&mut self) -> Result<PresentationMonitorId> {
        let id = PresentationMonitorId {
            instance: self.instance,
            sequence: self.mint()?,
        };
        self.presentation.slot = None;
        self.presentation.monitor = Some(Monitor {
            id,
            transitions: 0,
            samples: 0,
        });
        Ok(id)
    }

    /// Deliver every focus, selection and window transition in order, including
    /// away-and-back changes. The producer must serialize these with admission.
    /// A lost event is NOT a transition: call `lose_presentation_monitor`.
    pub fn note_presentation_transition(&mut self, id: PresentationMonitorId) -> Result<()> {
        let monitor = self
            .presentation
            .monitor
            .as_mut()
            .filter(|monitor| monitor.id == id)
            .ok_or(Error::Unavailable)?;
        let Some(next) = monitor.transitions.checked_add(1) else {
            self.stop_all_presentation_monitoring();
            return Err(Error::Unavailable);
        };
        monitor.transitions = next;
        if self
            .presentation
            .slot
            .is_some_and(|slot| matches!(slot.phase, Phase::Confirmed { .. }))
        {
            self.presentation.slot = None;
        }
        Ok(())
    }

    /// Any lost, unordered, or unknown native event closes the source and all
    /// evidence. Later focus samples cannot silently restore that source.
    pub fn lose_presentation_monitor(&mut self, id: PresentationMonitorId) -> Result<()> {
        if self
            .presentation
            .monitor
            .is_none_or(|monitor| monitor.id != id)
        {
            return Err(Error::Unavailable);
        }
        self.stop_all_presentation_monitoring();
        Ok(())
    }

    /// Fail closed when the host cannot establish which native source, event,
    /// or barrier is current. No later sample can restore the old source.
    pub fn invalidate_presentation_monitoring(&mut self) {
        self.stop_all_presentation_monitoring();
    }

    /// Record a fresh native-barrier observation. Only a qualified host may
    /// construct `Foreground`: sampling a currently active window is
    /// insufficient unless the source also proves no transition was missed.
    pub fn observe_presentation(
        &mut self,
        id: PresentationMonitorId,
        state: NativePresentationState,
    ) -> Result<Option<NativePresentationObservation>> {
        let monitor = self
            .presentation
            .monitor
            .as_mut()
            .filter(|monitor| monitor.id == id)
            .ok_or(Error::Unavailable)?;
        let Some(sample) = monitor.samples.checked_add(1) else {
            self.stop_all_presentation_monitoring();
            return Err(Error::Unavailable);
        };
        monitor.samples = sample;
        match state {
            NativePresentationState::Unknown => {
                self.stop_all_presentation_monitoring();
                Err(Error::Unavailable)
            }
            NativePresentationState::ForegroundRefused => {
                self.presentation.slot = None;
                Err(Error::ForegroundRefused)
            }
            NativePresentationState::Inactive
            | NativePresentationState::Foreground { selected: None, .. } => {
                if self
                    .presentation
                    .slot
                    .is_some_and(|slot| matches!(slot.phase, Phase::Confirmed { .. }))
                {
                    self.presentation.slot = None;
                }
                Ok(None)
            }
            NativePresentationState::Foreground {
                window,
                selected: Some(selected),
                workspace_revision,
            } => {
                if workspace_revision == u64::MAX {
                    self.stop_all_presentation_monitoring();
                    return Err(Error::Unavailable);
                }
                let observation = NativePresentationObservation {
                    monitor: id,
                    transitions: monitor.transitions,
                    sample,
                    window,
                    selected,
                    workspace_revision,
                };
                if self.presentation.slot.is_some_and(|slot| {
                    matches!(
                        slot.phase,
                        Phase::Confirmed { workspace_revision: old, .. }
                            if old != workspace_revision
                                || slot.attempt.window != window
                                || slot.attempt.target != selected
                    )
                }) {
                    self.presentation.slot = None;
                }
                Ok(Some(observation))
            }
        }
    }

    /// Prepare one attempt against a separately granted exact enrollment.
    /// `window` must come from the application's current target mapping, not
    /// from a client. No tab or window is changed by this method.
    pub fn prepare_presentation(
        &mut self,
        connection: ConnectionId,
        registration: ControllerRegistrationId,
        enrollment: ControllerEnrollmentId,
        window: PresentationWindowId,
        timeout: Duration,
        now: Instant,
    ) -> Result<PresentationAttempt> {
        self.poll_expiry(now);
        let grant = self.presentation_grant(connection, registration, enrollment, now)?;
        let monitor = self.presentation.monitor.ok_or(Error::Unavailable)?;
        if timeout.is_zero() || timeout > MAX_PRESENTATION_WAIT {
            return Err(Error::InvalidRequest);
        }
        if self
            .presentation
            .slot
            .is_some_and(|slot| matches!(slot.phase, Phase::Prepared | Phase::Waiting { .. }))
        {
            return Err(Error::Busy);
        }
        if grant.attempts >= MAX_PRESENTATION_ATTEMPTS_PER_GRANT
            || self.presentation.attempts >= MAX_PRESENTATION_ATTEMPTS_PER_BROKER
        {
            return Err(Error::QuotaExceeded);
        }
        let grant_expires = self
            .principals
            .get(&connection)
            .and_then(|principal| principal.grant.as_ref())
            .map_or(grant.expires, |ordinary| {
                grant.expires.min(ordinary.expires)
            });
        let deadline = now
            .checked_add(timeout)
            .ok_or(Error::InvalidRequest)?
            .min(grant_expires);
        let attempt = PresentationAttempt {
            connection,
            id: self.mint()?,
            target: grant.target,
            window,
            deadline,
        };
        let record = self
            .principals
            .get_mut(&connection)
            .and_then(|principal| principal.controller_registrations.get_mut(&registration))
            .and_then(|record| record.presentation_grant.as_mut())
            .ok_or(Error::Revoked)?;
        record.attempts += 1;
        self.presentation.attempts += 1;
        self.presentation.slot = Some(Slot {
            attempt,
            registration,
            enrollment,
            grant_epoch: grant.epoch,
            grant_expires,
            monitor: monitor.id,
            phase: Phase::Prepared,
        });
        Ok(attempt)
    }

    fn presentation_slot(
        &self,
        connection: ConnectionId,
        attempt: PresentationAttempt,
    ) -> Result<Slot> {
        if attempt.connection != connection {
            return Err(Error::Unauthorized);
        }
        self.presentation
            .slot
            .filter(|slot| slot.attempt == attempt)
            .ok_or(Error::Closed)
    }

    /// Called only after the application has selected the exact tab through its
    /// command model and queued the OS activation request. This is not success.
    pub fn presentation_requested(
        &mut self,
        connection: ConnectionId,
        attempt: PresentationAttempt,
        now: Instant,
    ) -> Result<()> {
        let previous_slot = self.presentation_slot(connection, attempt);
        self.poll_expiry(now);
        previous_slot?;
        if now >= attempt.deadline {
            return Err(Error::Expired);
        }
        let slot = self.presentation_slot(connection, attempt)?;
        let grant = self.presentation_grant(connection, slot.registration, slot.enrollment, now)?;
        if grant.epoch != slot.grant_epoch || !matches!(slot.phase, Phase::Prepared) {
            return Err(Error::Revoked);
        }
        let monitor = self
            .presentation
            .monitor
            .filter(|monitor| monitor.id == slot.monitor)
            .ok_or(Error::Unavailable)?;
        self.presentation.slot.as_mut().ok_or(Error::Closed)?.phase = Phase::Waiting {
            after_sample: monitor.samples,
        };
        Ok(())
    }

    /// Poll without blocking. `None` remains pending; a queued command alone
    /// cannot produce success. A confirmation requires a later fresh barrier.
    pub fn finish_presentation(
        &mut self,
        connection: ConnectionId,
        attempt: PresentationAttempt,
        observation: Option<NativePresentationObservation>,
        now: Instant,
    ) -> Result<Option<ConfirmedPresentation>> {
        if attempt.connection != connection {
            return Err(Error::Unauthorized);
        }
        self.poll_expiry(now);
        if now >= attempt.deadline {
            if self
                .presentation
                .slot
                .is_some_and(|slot| slot.attempt == attempt)
            {
                self.presentation.slot = None;
            }
            return Err(Error::Expired);
        }
        let slot = self.presentation_slot(connection, attempt)?;
        let grant = self.presentation_grant(connection, slot.registration, slot.enrollment, now)?;
        if grant.epoch != slot.grant_epoch {
            self.presentation.slot = None;
            return Err(Error::Revoked);
        }
        let Phase::Waiting { after_sample } = slot.phase else {
            return Err(Error::InvalidRequest);
        };
        let Some(observation) = observation else {
            return Ok(None);
        };
        let monitor = self
            .presentation
            .monitor
            .filter(|monitor| monitor.id == slot.monitor)
            .ok_or(Error::Unavailable)?;
        if observation.monitor != monitor.id
            || observation.transitions != monitor.transitions
            || observation.sample != monitor.samples
            || observation.sample <= after_sample
        {
            self.presentation.slot = None;
            return Err(Error::Revoked);
        }
        if observation.window != attempt.window || observation.selected != attempt.target {
            return Ok(None);
        }
        let revision = PresentationRevision {
            instance: self.instance,
            sequence: self.mint()?,
        };
        self.presentation.slot.as_mut().ok_or(Error::Closed)?.phase = Phase::Confirmed {
            revision,
            transitions: observation.transitions,
            workspace_revision: observation.workspace_revision,
            last_sample: observation.sample,
        };
        Ok(Some(ConfirmedPresentation {
            target: attempt.target,
            revision,
        }))
    }

    pub fn cancel_presentation(
        &mut self,
        connection: ConnectionId,
        attempt: PresentationAttempt,
    ) -> Result<()> {
        let slot = self.presentation_slot(connection, attempt)?;
        if matches!(slot.phase, Phase::Confirmed { .. }) {
            return Err(Error::Closed);
        }
        self.presentation.slot = None;
        Ok(())
    }

    /// Earliest wait or grant deadline, for a bounded host wakeup. Admissions
    /// independently check time even if a hidden window misses the wakeup.
    pub fn next_presentation_deadline(&self) -> Option<Instant> {
        self.presentation.slot.map(|slot| match slot.phase {
            Phase::Confirmed { .. } => slot.grant_expires,
            Phase::Prepared | Phase::Waiting { .. } => slot.attempt.deadline,
        })
    }

    pub(super) fn check_presentation(
        &self,
        connection: ConnectionId,
        requirement: PresentationRequirement,
        target: TargetRef,
        now: Instant,
    ) -> Result<()> {
        let grant = self.presentation_grant(
            connection,
            requirement.registration,
            requirement.enrollment,
            now,
        )?;
        let slot = self.presentation.slot.ok_or(Error::Revoked)?;
        let Phase::Confirmed {
            revision,
            transitions,
            ..
        } = slot.phase
        else {
            return Err(Error::Revoked);
        };
        if slot.attempt.connection != connection
            || slot.registration != requirement.registration
            || slot.enrollment != requirement.enrollment
            || slot.attempt.target != target
            || grant.target != target
            || grant.epoch != slot.grant_epoch
            || revision != requirement.revision
        {
            return Err(Error::Revoked);
        }
        if self
            .presentation
            .monitor
            .is_none_or(|monitor| monitor.id != slot.monitor || monitor.transitions != transitions)
        {
            return Err(Error::Revoked);
        }
        Ok(())
    }

    /// Revalidate at response or input admission, not when queueing. The host
    /// must obtain a new qualified native barrier and check its privacy,
    /// connection, attachment and human-input fences under that same boundary.
    pub fn revalidate_presentation(
        &mut self,
        connection: ConnectionId,
        requirement: PresentationRequirement,
        target: TargetRef,
        observation: NativePresentationObservation,
        now: Instant,
    ) -> Result<()> {
        self.poll_expiry(now);
        self.check_presentation(connection, requirement, target, now)?;
        let slot = self.presentation.slot.ok_or(Error::Revoked)?;
        let Phase::Confirmed {
            transitions,
            workspace_revision,
            last_sample,
            ..
        } = slot.phase
        else {
            return Err(Error::Revoked);
        };
        let monitor = self.presentation.monitor.ok_or(Error::Unavailable)?;
        if observation.monitor != monitor.id
            || observation.transitions != transitions
            || observation.transitions != monitor.transitions
            || observation.sample != monitor.samples
            || observation.sample <= last_sample
            || observation.workspace_revision != workspace_revision
            || observation.window != slot.attempt.window
            || observation.selected != target
        {
            self.presentation.slot = None;
            return Err(Error::Revoked);
        }
        if let Some(Slot {
            phase: Phase::Confirmed { last_sample, .. },
            ..
        }) = self.presentation.slot.as_mut()
        {
            *last_sample = observation.sample;
        }
        Ok(())
    }

    pub(super) fn cancel_presentation_for_connection(&mut self, connection: ConnectionId) {
        if self
            .presentation
            .slot
            .is_some_and(|slot| slot.attempt.connection == connection)
        {
            self.presentation.slot = None;
        }
    }

    pub(super) fn cancel_presentation_for_registration(
        &mut self,
        connection: ConnectionId,
        registration: ControllerRegistrationId,
    ) {
        if self.presentation.slot.is_some_and(|slot| {
            slot.attempt.connection == connection && slot.registration == registration
        }) {
            self.presentation.slot = None;
        }
    }

    pub(super) fn cancel_presentation_for_target(&mut self, target: TargetRef) {
        if self
            .presentation
            .slot
            .is_some_and(|slot| slot.attempt.target == target)
        {
            self.presentation.slot = None;
        }
    }

    pub(super) fn stop_all_presentation_monitoring(&mut self) {
        self.presentation.monitor = None;
        self.presentation.slot = None;
    }

    pub(super) fn expire_presentations(&mut self, now: Instant) {
        for principal in self.principals.values_mut() {
            for record in principal.controller_registrations.values_mut() {
                if record
                    .presentation_grant
                    .is_some_and(|grant| now >= grant.expires)
                {
                    record.presentation_grant = None;
                }
            }
        }
        if self.presentation.slot.is_some_and(|slot| {
            now >= slot.grant_expires
                || (!matches!(slot.phase, Phase::Confirmed { .. }) && now >= slot.attempt.deadline)
        }) {
            self.presentation.slot = None;
        }
    }
}
