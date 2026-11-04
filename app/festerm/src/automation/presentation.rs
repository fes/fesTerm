use super::*;
use festerm_automation::{
    ApprovalId, ControllerEnrollmentId, ControllerRegistrationId, Error, LeaseRef,
    MetadataGrantRequest, NativeControlGrant, NativeMetadataApproval, NativeMetadataGrant,
    NativePresentationObservation, PresentationAttempt, PresentationWindowId,
};
use festerm_session::{
    AutomationAdmissionGuard, AutomationAdmissionTryError, CommitAutomationInputError,
    ReserveAutomationInputError, ReservedAutomationInput, Session,
};
use sha2::Digest as _;
use std::{cell::Cell, rc::Rc};

use crate::session_controller::{AutomationInputFence, SessionController};

/// Host-validated input. This is not a native decision or an IPC credential.
#[allow(dead_code)]
pub(crate) enum DirectAction<'a> {
    Text { text: &'a str, submit: bool },
    Keys(&'a [String]),
}

struct FrozenDirectAction {
    binding: InvocationBinding,
    input: festerm_core::PreparedInput,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ReserveDirectInputError {
    Authority(Error),
    Backend(ReserveAutomationInputError),
}

pub(super) struct DirectInputReservation {
    reservation: ReservedAutomationInput,
    channel: u64,
    request: RequestId,
}

#[cfg(test)]
impl DirectInputReservation {
    pub(super) fn for_test(
        reservation: ReservedAutomationInput,
        channel: u64,
        request: RequestId,
    ) -> Self {
        Self {
            reservation,
            channel,
            request,
        }
    }
}

/// Shared by every window on the UI thread. The `Rc` prevents moving an input
/// barrier to another thread while UI selection can change. `u64::MAX`
/// permanently closes presentation rather than permitting a wrapped revision.
#[derive(Clone, Default)]
pub(crate) struct WorkspaceRevision(Rc<Cell<u64>>);

impl WorkspaceRevision {
    pub(crate) fn changed(&self) {
        self.0.set(self.0.get().saturating_add(1));
    }

    pub(crate) fn current(&self) -> Option<u64> {
        let current = self.0.get();
        (current != u64::MAX).then_some(current)
    }
}

/// A future platform implementation must retain its native ordering barrier
/// through the entire callback, including publication of the reserved input.
/// It must detect every native transition or gap; a current focus snapshot is
/// not sufficient. Acquiring the barrier and running its callback must be
/// nonblocking on the UI thread; a pending activation must be resumed by
/// events/deadline instead of waiting here. The callback must not re-enter UI
/// mutation. No production implementation is qualified yet.
#[allow(dead_code)]
pub(super) trait NativePresentationBarrier {
    fn with_observation<T>(
        &mut self,
        broker: &mut Broker,
        workspace_revision: u64,
        operation: impl FnOnce(&mut Broker, NativePresentationObservation) -> T,
    ) -> festerm_automation::Result<T>;
}

pub(super) struct BoundControlHandoff {
    target: TargetRef,
    grant: NativeControlGrant,
    fence: AutomationInputFence,
    sources: Vec<FrozenSource>,
    lease: Option<LeaseRef>,
    direct_action: Option<FrozenDirectAction>,
}

pub(super) struct BoundControlReview {
    pub(super) principal: ConnectionId,
    target: TargetRef,
    fence: AutomationInputFence,
    sources: Vec<FrozenSource>,
    metadata: Option<NativeMetadataGrant>,
    direct_action: Option<FrozenDirectAction>,
}

pub(super) struct BoundMetadataGrant {
    pub(super) grant: NativeMetadataGrant,
    sources: Vec<FrozenSource>,
}

pub(super) struct BoundMetadataReview {
    pub(super) principal: ConnectionId,
    sources: Vec<FrozenSource>,
    control: Option<(TargetRef, NativeControlGrant)>,
    control_sources: BTreeSet<TargetRef>,
}

struct FrozenSource {
    reference: TargetRef,
    controller_instance: u64,
    controller_generation: u64,
    guard: ipc::TargetGuard,
}

impl FrozenSource {
    fn is_current(
        &self,
        targets: &BTreeMap<u64, Target>,
        _guard: &AutomationAdmissionGuard,
    ) -> bool {
        self.guard.is_valid()
            && targets
                .get(&self.reference.session())
                .is_some_and(|target| {
                    target.reference == self.reference
                        && target.controller_instance == self.controller_instance
                        && target.controller_generation == self.controller_generation
                        && Arc::ptr_eq(&target.liveness, &self.guard.liveness)
                        && Arc::ptr_eq(&target.registration, &self.guard.registration)
                        && Arc::ptr_eq(&target.attachment, &self.guard.attachment)
                        && target.attachment_generation == self.guard.generation
                })
    }
}

/// Host-only proposal to be shown by native UI. It is not a decision or a
/// serializable authority, and its original fence remains in the coordinator.
pub(crate) struct PendingControlReview {
    principal: ConnectionId,
    target: TargetRef,
    approval_id: ApprovalId,
    request: GrantRequest,
    lifetime: Duration,
    metadata_sources: BTreeSet<TargetRef>,
}

/// A native decision for one review, never a caller-provided credential.
pub(crate) struct NativeControlDecision {
    principal: ConnectionId,
    target: TargetRef,
    approval_id: ApprovalId,
}

/// Host-only proposal for a separately approved metadata slot.
pub(crate) struct PendingMetadataReview {
    principal: ConnectionId,
    approval_id: ApprovalId,
    request: MetadataGrantRequest,
    lifetime: Duration,
    control: Option<TargetRef>,
    control_sources: BTreeSet<TargetRef>,
}

/// A native decision for one displayed metadata review.
pub(crate) struct NativeMetadataDecision {
    principal: ConnectionId,
    approval_id: ApprovalId,
}

#[allow(dead_code)]
impl PendingControlReview {
    pub(crate) fn target(&self) -> TargetRef {
        self.target
    }

    pub(crate) fn request(&self) -> &GrantRequest {
        &self.request
    }

    pub(crate) fn lifetime(&self) -> Duration {
        self.lifetime
    }

    /// Independently authorized metadata sources included in this handoff.
    pub(crate) fn metadata_sources(&self) -> &BTreeSet<TargetRef> {
        &self.metadata_sources
    }

    /// Only a native consent callback for this displayed review may call this.
    /// The decision accepts the full readable-source closure in that review.
    pub(crate) fn accept_from_native(&self) -> NativeControlDecision {
        NativeControlDecision {
            principal: self.principal,
            target: self.target,
            approval_id: self.approval_id,
        }
    }
}

#[allow(dead_code)]
impl PendingMetadataReview {
    pub(crate) fn request(&self) -> &MetadataGrantRequest {
        &self.request
    }

    pub(crate) fn lifetime(&self) -> Duration {
        self.lifetime
    }

    /// The currently approved control sink, if this metadata can flow to one.
    pub(crate) fn control_target(&self) -> Option<TargetRef> {
        self.control
    }

    /// Full previously reviewed control closure, not an additional read grant.
    pub(crate) fn control_sources(&self) -> &BTreeSet<TargetRef> {
        &self.control_sources
    }

    /// Only a native consent callback for this exact review may call this.
    pub(crate) fn accept_from_native(&self) -> NativeMetadataDecision {
        NativeMetadataDecision {
            principal: self.principal,
            approval_id: self.approval_id,
        }
    }
}

fn try_admission() -> festerm_automation::Result<AutomationAdmissionGuard> {
    festerm_session::try_automation_admission_lock().map_err(|error| match error {
        AutomationAdmissionTryError::Busy => Error::Busy,
        AutomationAdmissionTryError::Poisoned => Error::Unavailable,
    })
}

impl Coordinator {
    fn admission_principal_locked(
        &mut self,
        channel: u64,
        guard: &AutomationAdmissionGuard,
    ) -> festerm_automation::Result<ConnectionId> {
        if self.privacy_gate.is_closed() {
            if let Some(broker) = self.broker.as_mut() {
                broker.revoke_all(guard);
            }
            self.control_handoffs.clear();
            self.pending_control_reviews.clear();
            self.metadata_grants.clear();
            self.pending_metadata_reviews.clear();
            return Err(Error::Revoked);
        }
        let peer = self.peers.get(&channel).ok_or(Error::Closed)?;
        let principal = peer.principal;
        if peer.connection.is_closed() || self.stop.load(Ordering::Acquire) {
            if let Some(broker) = self.broker.as_mut() {
                broker.disconnect(principal, guard);
            }
            self.control_handoffs.remove(&principal);
            self.pending_control_reviews
                .retain(|_, review| review.principal != principal);
            self.metadata_grants.remove(&principal);
            self.pending_metadata_reviews
                .retain(|_, review| review.principal != principal);
            return Err(Error::Revoked);
        }
        Ok(principal)
    }

    fn live_control_fence_locked(
        &self,
        target: TargetRef,
        fence: &AutomationInputFence,
        guard: &AutomationAdmissionGuard,
    ) -> bool {
        fence.is_current()
            && self.live_presentation_target(target, guard)
            && self.targets.get(&target.session()).is_some_and(|current| {
                fence.matches_attachment(
                    current.controller_instance,
                    &current.attachment,
                    current.attachment_generation,
                )
            })
    }

    fn freeze_readable_sources_locked(
        &self,
        target: TargetRef,
        request: &GrantRequest,
        guard: &AutomationAdmissionGuard,
    ) -> festerm_automation::Result<Vec<FrozenSource>> {
        let scope = &request.scope;
        let limit = festerm_automation::MAX_SCOPE_TARGETS;
        if scope.discover.len() > limit
            || scope.observe.len() > limit
            || scope.discover.union(&scope.observe).count()
                + usize::from(!scope.discover.contains(&target) && !scope.observe.contains(&target))
                > limit
        {
            return Err(Error::InvalidRequest);
        }
        let sources = scope.discover.union(&scope.observe).copied().collect();
        self.freeze_sources_locked(&sources, guard)
    }

    fn freeze_sources_locked(
        &self,
        references: &BTreeSet<TargetRef>,
        guard: &AutomationAdmissionGuard,
    ) -> festerm_automation::Result<Vec<FrozenSource>> {
        references
            .iter()
            .map(|reference| {
                let current = self
                    .targets
                    .get(&reference.session())
                    .filter(|current| current.reference == *reference)
                    .ok_or(Error::StaleTarget)?;
                let source = FrozenSource {
                    reference: *reference,
                    controller_instance: current.controller_instance,
                    controller_generation: current.controller_generation,
                    guard: current.guard(),
                };
                if !source.is_current(&self.targets, guard) {
                    return Err(Error::StaleTarget);
                }
                Ok(source)
            })
            .collect()
    }

    fn metadata_snapshot_locked(
        &self,
        principal: ConnectionId,
        now: Instant,
        guard: &AutomationAdmissionGuard,
    ) -> festerm_automation::Result<Option<(NativeMetadataGrant, BTreeSet<TargetRef>)>> {
        let broker = self.broker.as_ref().ok_or(Error::Unavailable)?;
        let grant = match broker.native_metadata_grant(principal, now) {
            Ok(grant) => grant,
            Err(Error::Revoked | Error::Expired) => return Ok(None),
            Err(error) => return Err(error),
        };
        let bound = self
            .metadata_grants
            .get(&principal)
            .filter(|bound| bound.grant == grant)
            .ok_or(Error::Revoked)?;
        if !bound
            .sources
            .iter()
            .all(|source| source.is_current(&self.targets, guard))
        {
            return Err(Error::Revoked);
        }
        Ok(Some((
            grant,
            bound
                .sources
                .iter()
                .map(|source| source.reference)
                .collect(),
        )))
    }

    /// Cuts stale handoffs and native reviews before a new proposal snapshots
    /// broker ownership. Human takeover affects controllers of the exact
    /// target; source retirement affects only scopes containing that source.
    fn synchronize_control_handoffs_locked(
        &mut self,
        now: Instant,
        guard: &AutomationAdmissionGuard,
    ) -> festerm_automation::Result<()> {
        let broker = self.broker.as_mut().ok_or(Error::Unavailable)?;
        broker.poll_expiry(now);
        self.pending_control_reviews
            .retain(|id, review| broker.pending_approval_is_active(review.principal, *id, now));
        self.pending_metadata_reviews
            .retain(|id, review| broker.pending_approval_is_active(review.principal, *id, now));
        self.metadata_grants.retain(|principal, bound| {
            broker.native_metadata_grant(*principal, now) == Ok(bound.grant)
        });
        // A metadata slot installed outside this host seam has no proven
        // native review or attachment binding. Do not retain a control grant
        // that could consume it; leave the independently owned metadata alone.
        let unbound: BTreeSet<_> = self
            .control_handoffs
            .keys()
            .copied()
            .chain(
                self.pending_control_reviews
                    .values()
                    .map(|review| review.principal),
            )
            .filter(|principal| {
                broker.native_metadata_grant(*principal, now).is_ok()
                    && !self.metadata_grants.contains_key(principal)
            })
            .collect();
        for principal in unbound {
            broker.revoke_ordinary(principal);
        }
        self.control_handoffs.retain(|principal, handoff| {
            broker.native_control_grant(*principal, handoff.target, now) == Ok(handoff.grant)
        });
        self.pending_control_reviews
            .retain(|id, review| broker.pending_approval_is_active(review.principal, *id, now));
        self.pending_metadata_reviews
            .retain(|id, review| broker.pending_approval_is_active(review.principal, *id, now));
        let stale_sources: BTreeSet<_> = self
            .control_handoffs
            .values()
            .flat_map(|handoff| &handoff.sources)
            .chain(
                self.pending_control_reviews
                    .values()
                    .flat_map(|review| &review.sources),
            )
            .chain(
                self.metadata_grants
                    .values()
                    .flat_map(|grant| &grant.sources),
            )
            .chain(
                self.pending_metadata_reviews
                    .values()
                    .flat_map(|review| &review.sources),
            )
            .filter_map(|source| {
                if source.is_current(&self.targets, guard) {
                    None
                } else {
                    source.guard.registration.store(false, Ordering::Release);
                    Some(source.reference)
                }
            })
            .collect();
        let stale: BTreeSet<_> = self
            .control_handoffs
            .values()
            .map(|handoff| (handoff.target, &handoff.fence))
            .chain(
                self.pending_control_reviews
                    .values()
                    .map(|review| (review.target, &review.fence)),
            )
            .filter(|(target, fence)| !self.live_control_fence_locked(*target, fence, guard))
            .map(|(target, _)| target)
            .collect();
        let broker = self.broker.as_mut().ok_or(Error::Unavailable)?;
        for source in &stale_sources {
            if self
                .targets
                .get(&source.session())
                .is_some_and(|target| target.reference == *source)
            {
                let target = self
                    .targets
                    .remove(&source.session())
                    .expect("target was checked");
                target.registration.store(false, Ordering::Release);
            }
            match broker.close_target(*source, guard) {
                Ok(()) => {
                    if broker.forget_closed_target(*source).is_err() {
                        tracing::warn!("automation source retirement refused");
                    }
                }
                // Closing or replacing a target already cuts its grants and
                // proposals. Its successor must not be retired here.
                Err(Error::StaleTarget) => {}
                Err(_) => {
                    broker.revoke_all(guard);
                    self.control_handoffs.clear();
                    self.pending_control_reviews.clear();
                    self.metadata_grants.clear();
                    self.pending_metadata_reviews.clear();
                    return Err(Error::Revoked);
                }
            }
        }
        for target in &stale {
            match broker.human_takeover(*target) {
                Ok(()) => {}
                // Broker close/replacement already cuts every grant and
                // proposal containing this exact retired target.
                Err(Error::StaleTarget) => {}
                Err(_) => {
                    broker.revoke_all(guard);
                    self.control_handoffs.clear();
                    self.pending_control_reviews.clear();
                    self.metadata_grants.clear();
                    self.pending_metadata_reviews.clear();
                    return Err(Error::Revoked);
                }
            }
        }
        self.pending_control_reviews.retain(|id, review| {
            if stale.contains(&review.target)
                || review
                    .sources
                    .iter()
                    .any(|source| stale_sources.contains(&source.reference))
            {
                let _ = broker.reject_approval(review.principal, *id);
                false
            } else {
                broker.pending_approval_is_active(review.principal, *id, now)
            }
        });
        self.control_handoffs.retain(|principal, handoff| {
            !stale.contains(&handoff.target)
                && !handoff
                    .sources
                    .iter()
                    .any(|source| stale_sources.contains(&source.reference))
                && broker.native_control_grant(*principal, handoff.target, now) == Ok(handoff.grant)
        });
        self.pending_metadata_reviews
            .retain(|id, review| broker.pending_approval_is_active(review.principal, *id, now));
        self.metadata_grants.retain(|principal, bound| {
            broker.native_metadata_grant(*principal, now) == Ok(bound.grant)
        });
        Ok(())
    }

    /// Freeze the exact controller fence, readable-source guards and broker
    /// proposal before native UI presents this review.
    #[allow(dead_code)]
    pub(crate) fn prepare_control_handoff_review<S: Session>(
        &mut self,
        channel: u64,
        target: TargetRef,
        request: GrantRequest,
        lifetime: Duration,
        controller: &mut SessionController<S>,
    ) -> festerm_automation::Result<PendingControlReview> {
        let guard = try_admission()?;
        let principal = self.admission_principal_locked(channel, &guard)?;
        let now = Instant::now();
        self.synchronize_control_handoffs_locked(now, &guard)?;
        if self.peers[&channel].grant.is_some() {
            return Err(Error::Unavailable);
        }
        if request.scope.control.len() != 1 || !request.scope.control.contains(&target) {
            return Err(Error::InvalidRequest);
        }
        if lifetime.is_zero() || lifetime > festerm_automation::MAX_GRANT_LIFETIME {
            return Err(Error::InvalidRequest);
        }
        if !self.live_presentation_target(target, &guard) {
            return Err(Error::StaleTarget);
        }
        if self.pending_control_reviews.len() >= festerm_automation::MAX_PENDING_APPROVALS {
            return Err(Error::QuotaExceeded);
        }
        let current = self
            .targets
            .get(&target.session())
            .ok_or(Error::StaleTarget)?;
        let fence = controller
            .capture_automation_input_fence_locked(&guard)
            .ok_or(Error::Unavailable)?;
        if !fence.matches_attachment(
            current.controller_instance,
            &current.attachment,
            current.attachment_generation,
        ) {
            return Err(Error::BindingMismatch);
        }
        let sources = self.freeze_readable_sources_locked(target, &request, &guard)?;
        let metadata = self.metadata_snapshot_locked(principal, now, &guard)?;
        let metadata_sources = metadata
            .as_ref()
            .map_or_else(BTreeSet::new, |(_, sources)| sources.clone());
        let readable_sources: BTreeSet<_> = sources.iter().map(|source| source.reference).collect();
        if !metadata_sources.is_subset(&readable_sources) {
            return Err(Error::Unauthorized);
        }
        let approval_id = self
            .broker
            .as_mut()
            .ok_or(Error::Unavailable)?
            .request_approval(principal, request.clone(), now)?;
        self.pending_control_reviews.insert(
            approval_id,
            BoundControlReview {
                principal,
                target,
                fence,
                sources,
                metadata: metadata.map(|(grant, _)| grant),
                direct_action: None,
            },
        );
        Ok(PendingControlReview {
            principal,
            target,
            approval_id,
            request,
            lifetime,
            metadata_sources,
        })
    }

    /// Apply only the native decision made for the previously frozen review.
    /// A newly captured fence can never replace the one displayed to the user.
    #[allow(dead_code)]
    pub(crate) fn approve_control_handoff(
        &mut self,
        channel: u64,
        review: &PendingControlReview,
        decision: &NativeControlDecision,
    ) -> festerm_automation::Result<()> {
        let guard = try_admission()?;
        let principal = self.admission_principal_locked(channel, &guard)?;
        if review.principal != principal
            || decision.principal != principal
            || decision.target != review.target
            || decision.approval_id != review.approval_id
        {
            return Err(Error::Unauthorized);
        }
        let now = Instant::now();
        self.synchronize_control_handoffs_locked(now, &guard)?;
        if self.peers[&channel].grant.is_some() {
            return Err(Error::Unavailable);
        }
        let bound = self
            .pending_control_reviews
            .get(&review.approval_id)
            .ok_or(Error::Revoked)?;
        if bound.principal != principal || bound.target != review.target {
            return Err(Error::Unauthorized);
        }
        let metadata = self.metadata_snapshot_locked(principal, now, &guard)?;
        if metadata.as_ref().map(|(grant, _)| *grant) != bound.metadata
            || metadata
                .as_ref()
                .map_or_else(BTreeSet::new, |(_, sources)| sources.clone())
                != review.metadata_sources
        {
            return Err(Error::Revoked);
        }
        let readable_sources = review
            .request
            .scope
            .discover
            .union(&review.request.scope.observe)
            .copied()
            .collect();
        let approval = NativeApproval::new(review.request.clone(), review.lifetime)
            .accept_control_handoff(review.target, readable_sources);
        if !approval.is_single_control_handoff(review.target) {
            return Err(Error::Unauthorized);
        }
        if !self.live_control_fence_locked(review.target, &bound.fence, &guard)
            || !bound
                .sources
                .iter()
                .all(|source| source.is_current(&self.targets, &guard))
        {
            return Err(Error::Revoked);
        }
        if self.control_handoffs.len() >= festerm_automation::Limits::default().connections
            && !self.control_handoffs.contains_key(&principal)
        {
            return Err(Error::QuotaExceeded);
        }
        let now = Instant::now();
        let broker = self.broker.as_mut().ok_or(Error::Unavailable)?;
        if let Err(error) = broker.approve(principal, review.approval_id, approval, now) {
            let _ = broker.reject_approval(principal, review.approval_id);
            self.pending_control_reviews.remove(&review.approval_id);
            return Err(error);
        }
        let bound = self.pending_control_reviews.remove(&review.approval_id);
        self.pending_control_reviews
            .retain(|_, pending| pending.principal != principal);
        self.pending_metadata_reviews
            .retain(|id, pending| broker.pending_approval_is_active(pending.principal, *id, now));
        let Some(bound) = bound else {
            broker.revoke_ordinary(principal);
            self.control_handoffs.remove(&principal);
            return Err(Error::Revoked);
        };
        let grant = match broker.native_control_grant(principal, review.target, Instant::now()) {
            Ok(grant) => grant,
            Err(error) => {
                broker.revoke_ordinary(principal);
                self.control_handoffs.remove(&principal);
                return Err(error);
            }
        };
        self.control_handoffs.insert(
            principal,
            BoundControlHandoff {
                target: review.target,
                grant,
                fence: bound.fence,
                sources: bound.sources,
                lease: None,
                direct_action: bound.direct_action,
            },
        );
        Ok(())
    }

    /// Freeze both canonical parameters and core-encoded input before creating
    /// the native review. Only this host-owned path can bind a direct action
    /// to its original review and handoff.
    #[allow(dead_code)]
    pub(crate) fn prepare_direct_action_review(
        &mut self,
        channel: u64,
        target: TargetRef,
        action: DirectAction<'_>,
        lifetime: Duration,
        tab: &mut SessionTab,
    ) -> festerm_automation::Result<PendingControlReview> {
        self.prepare_direct_action_review_with_sources(
            channel,
            target,
            action,
            lifetime,
            tab,
            &Scope::default(),
        )
    }

    /// Explicit source scopes are reviewed as part of the ordinary handoff.
    /// They are never inferred from an independently granted metadata slot.
    #[allow(dead_code)]
    pub(crate) fn prepare_direct_action_review_with_sources(
        &mut self,
        channel: u64,
        target: TargetRef,
        action: DirectAction<'_>,
        lifetime: Duration,
        tab: &mut SessionTab,
        sources: &Scope,
    ) -> festerm_automation::Result<PendingControlReview> {
        let current = self
            .targets
            .get(&target.session())
            .ok_or(Error::StaleTarget)?;
        if current.reference != target
            || tab.controller.automation_instance() != Some(current.controller_instance)
        {
            return Err(Error::BindingMismatch);
        }
        let (input, canonical) = match action {
            DirectAction::Text { text, submit } => {
                if text.is_empty() || text.len() > festerm_automation::MAX_INPUT_BYTES as usize {
                    return Err(Error::InvalidRequest);
                }
                let mut events = vec![festerm_core::InputEvent::Paste(text.to_owned())];
                if submit {
                    events.push(festerm_core::InputEvent::Key(festerm_core::Key::Enter));
                }
                let input = tab
                    .terminal
                    .prepare_input(&events, festerm_automation::MAX_INPUT_BYTES as usize)
                    .map_err(|_| Error::InvalidRequest)?;
                (
                    input,
                    serde_json::json!({"version": 1, "operation": "send_text", "text": text, "submit": submit}),
                )
            }
            DirectAction::Keys(keys) => {
                let input = festerm_automation_workflow::terminal_input::prepare_send_keys(
                    &tab.terminal,
                    keys,
                )
                .map_err(|_| Error::InvalidRequest)?;
                (
                    input,
                    serde_json::json!({"version": 1, "operation": "send_keys", "keys": keys}),
                )
            }
        };
        let bytes = u32::try_from(input.bytes().len()).map_err(|_| Error::InvalidRequest)?;
        if bytes == 0 {
            return Err(Error::InvalidRequest);
        }
        let canonical = serde_json::to_vec(&canonical).map_err(|_| Error::InvalidRequest)?;
        let binding = InvocationBinding {
            api_version: 1,
            invocation: Invocation::DirectInput {
                target,
                input: Digest(Sha256::digest(input.bytes()).into()),
                bytes,
            },
            parameters: Digest(Sha256::digest(canonical).into()),
        };
        let request = GrantRequest {
            scope: Scope {
                control: BTreeSet::from([target]),
                discover: sources.discover.clone(),
                observe: sources.observe.clone(),
                ..Scope::default()
            },
            bindings: BTreeSet::from([binding]),
            direct_read_calls: BTreeSet::new(),
        };
        let review = self.prepare_control_handoff_review(
            channel,
            target,
            request,
            lifetime,
            &mut tab.controller,
        )?;
        let bound = self
            .pending_control_reviews
            .get_mut(&review.approval_id)
            .ok_or(Error::Revoked)?;
        bound.direct_action = Some(FrozenDirectAction { binding, input });
        Ok(review)
    }

    #[allow(dead_code)]
    pub(crate) fn cancel_control_handoff_review(
        &mut self,
        channel: u64,
        review: &PendingControlReview,
    ) -> festerm_automation::Result<()> {
        let guard = try_admission()?;
        let principal = self.admission_principal_locked(channel, &guard)?;
        if review.principal != principal {
            return Err(Error::Unauthorized);
        }
        let bound = self
            .pending_control_reviews
            .get(&review.approval_id)
            .ok_or(Error::Revoked)?;
        if bound.principal != principal || bound.target != review.target {
            return Err(Error::Unauthorized);
        }
        let result = self
            .broker
            .as_mut()
            .ok_or(Error::Unavailable)?
            .reject_approval(principal, review.approval_id);
        self.pending_control_reviews.remove(&review.approval_id);
        result
    }

    /// Freeze each exact metadata source and any existing control handoff
    /// before the native consent callback can display this proposal.
    #[allow(dead_code)]
    pub(crate) fn prepare_metadata_review(
        &mut self,
        channel: u64,
        request: MetadataGrantRequest,
        lifetime: Duration,
    ) -> festerm_automation::Result<PendingMetadataReview> {
        let guard = try_admission()?;
        let principal = self.admission_principal_locked(channel, &guard)?;
        let now = Instant::now();
        self.synchronize_control_handoffs_locked(now, &guard)?;
        // The observation-only adapter's queued frames lack per-grant
        // delivery fences; do not compose it with this dormant host seam.
        if self.peers[&channel].grant.is_some() {
            return Err(Error::Unavailable);
        }
        if request.targets.is_empty()
            || request.targets.len() > festerm_automation::MAX_SCOPE_TARGETS
            || lifetime.is_zero()
            || lifetime > festerm_automation::MAX_GRANT_LIFETIME
        {
            return Err(Error::InvalidRequest);
        }
        let sources = self.freeze_sources_locked(&request.targets, &guard)?;
        let control = self.control_handoffs.get(&principal).map(|handoff| {
            let readable: BTreeSet<_> = handoff
                .sources
                .iter()
                .map(|source| source.reference)
                .collect();
            (handoff.target, handoff.grant, readable)
        });
        if control
            .as_ref()
            .is_some_and(|(_, _, readable)| !request.targets.is_subset(readable))
        {
            return Err(Error::Unauthorized);
        }
        let control_sources = control
            .as_ref()
            .map_or_else(BTreeSet::new, |(_, _, sources)| sources.clone());
        let control = control.map(|(target, grant, _)| (target, grant));
        let approval_id = self
            .broker
            .as_mut()
            .ok_or(Error::Unavailable)?
            .request_metadata_approval(principal, request.clone(), now)?;
        self.pending_metadata_reviews.insert(
            approval_id,
            BoundMetadataReview {
                principal,
                sources,
                control,
                control_sources: control_sources.clone(),
            },
        );
        Ok(PendingMetadataReview {
            principal,
            approval_id,
            request,
            lifetime,
            control: control.map(|(target, _)| target),
            control_sources,
        })
    }

    /// Apply only the displayed metadata decision, including its exact
    /// source-to-control flow. It never replaces the ordinary slot.
    #[allow(dead_code)]
    pub(crate) fn approve_metadata_review(
        &mut self,
        channel: u64,
        review: &PendingMetadataReview,
        decision: &NativeMetadataDecision,
    ) -> festerm_automation::Result<()> {
        let guard = try_admission()?;
        let principal = self.admission_principal_locked(channel, &guard)?;
        if review.principal != principal
            || decision.principal != principal
            || decision.approval_id != review.approval_id
        {
            return Err(Error::Unauthorized);
        }
        let now = Instant::now();
        self.synchronize_control_handoffs_locked(now, &guard)?;
        if self.peers[&channel].grant.is_some() {
            return Err(Error::Unavailable);
        }
        let bound = self
            .pending_metadata_reviews
            .get(&review.approval_id)
            .ok_or(Error::Revoked)?;
        let current_control = self
            .control_handoffs
            .get(&principal)
            .map(|handoff| (handoff.target, handoff.grant));
        let current_sources: BTreeSet<_> = self
            .control_handoffs
            .get(&principal)
            .into_iter()
            .flat_map(|handoff| handoff.sources.iter().map(|source| source.reference))
            .collect();
        if bound.principal != principal
            || bound.control != current_control
            || review.control != bound.control.map(|(target, _)| target)
            || bound.control_sources != current_sources
            || review.control_sources != bound.control_sources
            || !bound
                .sources
                .iter()
                .all(|source| source.is_current(&self.targets, &guard))
        {
            return Err(Error::Revoked);
        }
        let mut approval = NativeMetadataApproval::new(review.request.clone(), review.lifetime);
        if let Some((target, _)) = bound.control {
            approval = approval.accept_control_disclosure(target, review.request.targets.clone());
        }
        let now = Instant::now();
        let broker = self.broker.as_mut().ok_or(Error::Unavailable)?;
        if let Err(error) = broker.approve_metadata(principal, review.approval_id, approval, now) {
            let _ = broker.reject_approval(principal, review.approval_id);
            self.pending_metadata_reviews.remove(&review.approval_id);
            return Err(error);
        }
        let bound = self.pending_metadata_reviews.remove(&review.approval_id);
        let grant = broker.native_metadata_grant(principal, Instant::now());
        let (Some(bound), Ok(grant)) = (bound, grant) else {
            broker.revoke_metadata(principal);
            self.metadata_grants.remove(&principal);
            return Err(Error::Revoked);
        };
        self.metadata_grants.insert(
            principal,
            BoundMetadataGrant {
                grant,
                sources: bound.sources,
            },
        );
        self.pending_control_reviews
            .retain(|id, pending| broker.pending_approval_is_active(pending.principal, *id, now));
        self.pending_metadata_reviews
            .retain(|id, pending| broker.pending_approval_is_active(pending.principal, *id, now));
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn cancel_metadata_review(
        &mut self,
        channel: u64,
        review: &PendingMetadataReview,
    ) -> festerm_automation::Result<()> {
        let guard = try_admission()?;
        let principal = self.admission_principal_locked(channel, &guard)?;
        if review.principal != principal
            || self
                .pending_metadata_reviews
                .get(&review.approval_id)
                .is_none_or(|bound| bound.principal != principal)
        {
            return Err(Error::Unauthorized);
        }
        let result = self
            .broker
            .as_mut()
            .ok_or(Error::Unavailable)?
            .reject_approval(principal, review.approval_id);
        self.pending_metadata_reviews.remove(&review.approval_id);
        result
    }

    #[allow(dead_code)]
    pub(crate) fn revoke_control_handoff(
        &mut self,
        channel: u64,
    ) -> festerm_automation::Result<()> {
        let guard = try_admission()?;
        let principal = self.admission_principal_locked(channel, &guard)?;
        if self.peers[&channel].grant.is_some() {
            return Err(Error::Unavailable);
        }
        let broker = self.broker.as_mut().ok_or(Error::Unavailable)?;
        broker.revoke_ordinary(principal);
        self.control_handoffs.remove(&principal);
        let now = Instant::now();
        self.pending_control_reviews
            .retain(|id, review| broker.pending_approval_is_active(review.principal, *id, now));
        self.pending_metadata_reviews
            .retain(|id, review| broker.pending_approval_is_active(review.principal, *id, now));
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn revoke_metadata_grant(&mut self, channel: u64) -> festerm_automation::Result<()> {
        let guard = try_admission()?;
        let principal = self.admission_principal_locked(channel, &guard)?;
        let broker = self.broker.as_mut().ok_or(Error::Unavailable)?;
        broker.revoke_metadata(principal);
        self.metadata_grants.remove(&principal);
        let now = Instant::now();
        self.pending_control_reviews
            .retain(|id, review| broker.pending_approval_is_active(review.principal, *id, now));
        self.pending_metadata_reviews
            .retain(|id, review| broker.pending_approval_is_active(review.principal, *id, now));
        Ok(())
    }

    /// A lease is associated with the original handoff, never a recaptured
    /// fence. The broker freezes this grant identity into the run as well.
    #[allow(dead_code)]
    pub(super) fn acquire_control_lease(
        &mut self,
        channel: u64,
        run: RunId,
        target: TargetRef,
    ) -> festerm_automation::Result<LeaseRef> {
        let guard = try_admission()?;
        let principal = self.admission_principal_locked(channel, &guard)?;
        let now = Instant::now();
        self.synchronize_control_handoffs_locked(now, &guard)?;
        let handoff = self
            .control_handoffs
            .get(&principal)
            .ok_or(Error::Revoked)?;
        if handoff.target != target {
            return Err(Error::BindingMismatch);
        }
        if !handoff
            .sources
            .iter()
            .all(|source| source.is_current(&self.targets, &guard))
        {
            return Err(Error::Revoked);
        }
        let broker = self.broker.as_mut().ok_or(Error::Unavailable)?;
        if broker.native_control_grant(principal, target, Instant::now())? != handoff.grant {
            return Err(Error::Revoked);
        }
        let transition = broker.prepare_lease(principal, run, target, Instant::now())?;
        let lease = broker.commit_lease(transition, Instant::now())?;
        self.control_handoffs
            .get_mut(&principal)
            .ok_or(Error::Revoked)?
            .lease = Some(lease);
        Ok(lease)
    }

    /// The application binds its one shared UI workspace before use. Rebinding
    /// could make an old revision match a different workspace, so it fails closed.
    pub(crate) fn bind_presentation_workspace(
        &mut self,
        workspace: &WorkspaceRevision,
    ) -> festerm_automation::Result<()> {
        if self.presentation_workspace.is_some() {
            self.invalidate_presentation_locked();
            return Err(Error::Unavailable);
        }
        self.presentation_workspace = Some(workspace.clone());
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn setup_presentation_for_test(
        &mut self,
        app: &FesTermApp,
        tab: TabId,
    ) -> (
        u64,
        ControllerRegistrationId,
        ControllerEnrollmentId,
        festerm_automation::PresentationMonitorId,
        TargetRef,
    ) {
        use festerm_automation::{
            NativeControllerEnrollment, NativeControllerPresentationApproval,
        };

        self.stop = Arc::new(AtomicBool::new(false));
        self.privacy_gate = Arc::new(PrivacyGate::new(false));
        assert!(self.privacy_gate.bind_mcp(&self.stop));
        let mut broker = Broker::new([43; 16]);
        self.reconcile(&mut broker, &[app]);
        let target = self.targets.get_mut(&tab.chip_id()).unwrap();
        // The fake SSH fixture stands in for a local backend only in this test.
        target.local_pty = true;
        let reference = target.reference;
        let principal = broker.accept_connection().unwrap();
        let guard = festerm_session::automation_admission_lock().unwrap();
        let registration = broker
            .register_controller_session(principal, b"controller", &guard)
            .unwrap();
        let proposal = broker
            .prepare_controller_enrollment(principal, registration, reference, &guard)
            .unwrap();
        let enrollment = broker
            .approve_controller_enrollment(
                principal,
                NativeControllerEnrollment::new(proposal),
                &guard,
            )
            .unwrap();
        drop(guard);
        let now = Instant::now();
        let proposal = broker
            .prepare_controller_presentation_approval(principal, registration, enrollment, now)
            .unwrap();
        broker
            .approve_controller_presentation(
                principal,
                NativeControllerPresentationApproval::new(proposal, Duration::from_secs(60)),
                now,
            )
            .unwrap();
        let monitor = broker.start_qualified_presentation_monitor().unwrap();
        let (connection, _receive) = ipc::test_connection(1);
        self.peers.insert(
            1,
            Peer {
                connection,
                principal,
                machine: Machine::new(),
                code: VerificationCode::new("0123456789abcdef").unwrap(),
                grant: None,
                waits: Vec::new(),
                controller: controller::PeerState::default(),
            },
        );
        self.broker = Some(broker);
        (1, registration, enrollment, monitor, reference)
    }

    #[cfg(test)]
    pub(crate) fn presentation_broker_for_test(
        &mut self,
        channel: u64,
    ) -> (ConnectionId, &mut Broker) {
        let principal = self.peers.get(&channel).unwrap().principal;
        (principal, self.broker.as_mut().unwrap())
    }

    /// An internal candidate only; the same association and the real target
    /// guards are rechecked under the admission lock before any UI command.
    pub(crate) fn controller_presentation_target(
        &self,
        channel: u64,
        registration: ControllerRegistrationId,
        enrollment: ControllerEnrollmentId,
    ) -> festerm_automation::Result<TargetRef> {
        let peer = self.peers.get(&channel).ok_or(Error::Closed)?;
        if peer.connection.is_closed()
            || self.stop.load(Ordering::Acquire)
            || self.privacy_gate.is_closed()
        {
            return Err(Error::Revoked);
        }
        self.broker
            .as_ref()
            .ok_or(Error::Unavailable)?
            .resolve_controller_enrollment(peer.principal, registration, enrollment)
    }

    fn live_presentation_target(
        &self,
        reference: TargetRef,
        _guard: &AutomationAdmissionGuard,
    ) -> bool {
        self.targets
            .get(&reference.session())
            .is_some_and(|target| {
                target.reference == reference
                    && target.local_pty
                    && target.liveness.load(Ordering::Acquire)
                    && target.registration.load(Ordering::Acquire)
                    && target.attachment.is_current(target.attachment_generation)
            })
    }

    /// The caller holds the shared admission lock until the exact tab command
    /// and focus request have been queued and `presentation_requested` returns.
    pub(crate) fn prepare_presentation_locked(
        &mut self,
        channel: u64,
        registration: ControllerRegistrationId,
        enrollment: ControllerEnrollmentId,
        destination: (TargetRef, PresentationWindowId),
        timeout: Duration,
        guard: &AutomationAdmissionGuard,
    ) -> festerm_automation::Result<PresentationAttempt> {
        let (target, window) = destination;
        if self.controller_presentation_target(channel, registration, enrollment)? != target
            || !self.live_presentation_target(target, guard)
        {
            return Err(Error::StaleTarget);
        }
        let principal = self.peers.get(&channel).ok_or(Error::Closed)?.principal;
        self.broker
            .as_mut()
            .ok_or(Error::Unavailable)?
            .prepare_presentation(
                principal,
                registration,
                enrollment,
                window,
                timeout,
                Instant::now(),
            )
    }

    pub(crate) fn presentation_requested_locked(
        &mut self,
        channel: u64,
        attempt: PresentationAttempt,
        now: Instant,
        guard: &AutomationAdmissionGuard,
    ) -> festerm_automation::Result<()> {
        let peer = self.peers.get(&channel).ok_or(Error::Closed)?;
        if peer.connection.is_closed()
            || self.stop.load(Ordering::Acquire)
            || self.privacy_gate.is_closed()
            || !self.live_presentation_target(attempt.target(), guard)
        {
            return Err(Error::Revoked);
        }
        self.broker
            .as_mut()
            .ok_or(Error::Unavailable)?
            .presentation_requested(peer.principal, attempt, now)
    }

    pub(crate) fn cancel_presentation_locked(
        &mut self,
        channel: u64,
        attempt: PresentationAttempt,
        _guard: &AutomationAdmissionGuard,
    ) {
        if let (Some(peer), Some(broker)) = (self.peers.get(&channel), self.broker.as_mut()) {
            let _ = broker.cancel_presentation(peer.principal, attempt);
        }
    }

    pub(crate) fn invalidate_presentation_locked(&mut self) {
        if let Some(broker) = self.broker.as_mut() {
            broker.invalidate_presentation_monitoring();
        }
    }

    /// Also driven by eframe's logic-only path when windows are hidden. The
    /// kernel independently checks every deadline at confirmation/admission.
    pub(crate) fn advance_presentation_deadline(&mut self, context: &egui::Context) {
        let Some(broker) = self.broker.as_mut() else {
            return;
        };
        if broker.next_presentation_deadline().is_none() {
            return;
        }
        let now = Instant::now();
        broker.poll_expiry(now);
        if let Some(deadline) = broker.next_presentation_deadline() {
            context.request_repaint_after(deadline.saturating_duration_since(now));
        }
    }

    /// This is deliberately not exposed by the observation MCP adapter. It is
    /// the future direct-input admission seam, not a native qualification.
    #[allow(dead_code)]
    pub(super) fn commit_presented_input<B: NativePresentationBarrier>(
        &mut self,
        channel: u64,
        request: RequestId,
        reservation: ReservedAutomationInput,
        barrier: &mut B,
    ) -> std::result::Result<(), CommitAutomationInputError<Error>> {
        self.commit_input(channel, request, reservation, None, Some(barrier))
    }

    fn validate_direct_input_locked(
        &mut self,
        channel: u64,
        request: RequestId,
        tab: &SessionTab,
        guard: &AutomationAdmissionGuard,
    ) -> festerm_automation::Result<&FrozenDirectAction> {
        let principal = self.admission_principal_locked(channel, guard)?;
        self.synchronize_control_handoffs_locked(Instant::now(), guard)?;
        let broker = self.broker.as_ref().ok_or(Error::Unavailable)?;
        let (lease, grant) = broker.prepared_control_binding(principal, request, Instant::now())?;
        let binding = broker.prepared_direct_input_binding(principal, request, Instant::now())?;
        let target = lease.target();
        let handoff = self
            .control_handoffs
            .get(&principal)
            .ok_or(Error::Revoked)?;
        if handoff.target != target || handoff.grant != grant || handoff.lease != Some(lease) {
            return Err(Error::BindingMismatch);
        }
        if !handoff
            .sources
            .iter()
            .all(|source| source.is_current(&self.targets, guard))
            || !self.live_presentation_target(target, guard)
        {
            return Err(Error::StaleTarget);
        }
        let current = self
            .targets
            .get(&target.session())
            .ok_or(Error::StaleTarget)?;
        let frozen = handoff
            .direct_action
            .as_ref()
            .ok_or(Error::BindingMismatch)?;
        let attachment = tab
            .controller
            .automation_attachment_epoch()
            .ok_or(Error::Unavailable)?;
        if frozen.binding != binding
            || tab.controller.automation_instance() != Some(current.controller_instance)
            || !Arc::ptr_eq(&attachment, &current.attachment)
            || !attachment.is_current(current.attachment_generation)
            || tab.terminal.modes() != frozen.input.modes()
        {
            return Err(Error::BindingMismatch);
        }
        Ok(frozen)
    }

    /// Reserve only the bytes frozen into the live handoff, on that tab's own
    /// backend. Backend reservation acquires the admission lock itself, so the
    /// checks on either side deliberately use separate lock acquisitions.
    #[allow(dead_code)]
    pub(super) fn reserve_direct_input(
        &mut self,
        channel: u64,
        request: RequestId,
        tab: &SessionTab,
    ) -> std::result::Result<DirectInputReservation, ReserveDirectInputError> {
        let guard = try_admission().map_err(ReserveDirectInputError::Authority)?;
        let frozen = self
            .validate_direct_input_locked(channel, request, tab, &guard)
            .map_err(ReserveDirectInputError::Authority)?;
        drop(guard);
        let reservation = tab
            .controller
            .reserve_automation_input(frozen.input.bytes())
            .map_err(ReserveDirectInputError::Backend)?;
        let guard = try_admission().map_err(ReserveDirectInputError::Authority)?;
        self.validate_direct_input_locked(channel, request, tab, &guard)
            .map_err(ReserveDirectInputError::Authority)?;
        let principal = self
            .admission_principal_locked(channel, &guard)
            .map_err(ReserveDirectInputError::Authority)?;
        let (lease, _) = self
            .broker
            .as_ref()
            .ok_or(ReserveDirectInputError::Authority(Error::Unavailable))?
            .prepared_control_binding(principal, request, Instant::now())
            .map_err(ReserveDirectInputError::Authority)?;
        let current = self
            .targets
            .get(&lease.target().session())
            .ok_or(ReserveDirectInputError::Authority(Error::StaleTarget))?;
        if !reservation.matches_attachment(
            current.controller_instance,
            &current.attachment,
            current.attachment_generation,
            &guard,
        ) {
            return Err(ReserveDirectInputError::Authority(Error::BindingMismatch));
        }
        Ok(DirectInputReservation {
            reservation,
            channel,
            request,
        })
    }

    /// Commits only the action frozen into this handoff. The borrowed tab
    /// keeps its terminal state stable through the reservation callback.
    #[allow(dead_code)]
    pub(super) fn commit_direct_input<B: NativePresentationBarrier>(
        &mut self,
        channel: u64,
        request: RequestId,
        reservation: DirectInputReservation,
        tab: &SessionTab,
        barrier: Option<&mut B>,
    ) -> std::result::Result<(), CommitAutomationInputError<Error>> {
        if reservation.channel != channel || reservation.request != request {
            return Err(CommitAutomationInputError::Refused(Error::BindingMismatch));
        }
        self.commit_input(
            channel,
            request,
            reservation.reservation,
            Some(tab),
            barrier,
        )
    }

    fn commit_input<B: NativePresentationBarrier>(
        &mut self,
        channel: u64,
        request: RequestId,
        reservation: ReservedAutomationInput,
        tab: Option<&SessionTab>,
        barrier: Option<&mut B>,
    ) -> std::result::Result<(), CommitAutomationInputError<Error>> {
        let refused = CommitAutomationInputError::Refused;
        let guard = try_admission().map_err(refused)?;
        let principal = self
            .admission_principal_locked(channel, &guard)
            .map_err(refused)?;
        self.synchronize_control_handoffs_locked(Instant::now(), &guard)
            .map_err(refused)?;
        let broker = self
            .broker
            .as_ref()
            .ok_or_else(|| refused(Error::Unavailable))?;
        let (lease, grant) = broker
            .prepared_control_binding(principal, request, Instant::now())
            .map_err(refused)?;
        let target = lease.target();
        let handoff = self
            .control_handoffs
            .get(&principal)
            .ok_or_else(|| refused(Error::Revoked))?;
        if handoff.target != target || handoff.grant != grant || handoff.lease != Some(lease) {
            return Err(refused(Error::BindingMismatch));
        }
        if !handoff
            .sources
            .iter()
            .all(|source| source.is_current(&self.targets, &guard))
        {
            return Err(refused(Error::Revoked));
        }
        if !self.live_presentation_target(target, &guard) {
            if let Some(broker) = self.broker.as_mut() {
                let _ = broker.close_target(target, &guard);
            }
            return Err(refused(Error::StaleTarget));
        }
        let current = self
            .targets
            .get(&target.session())
            .ok_or_else(|| refused(Error::StaleTarget))?;
        let direct_binding =
            broker.prepared_direct_input_binding(principal, request, Instant::now());
        let frozen = match (direct_binding, tab) {
            (Ok(binding), Some(tab)) => {
                let frozen = handoff
                    .direct_action
                    .as_ref()
                    .ok_or_else(|| refused(Error::BindingMismatch))?;
                if frozen.binding != binding
                    || tab.controller.automation_instance() != Some(current.controller_instance)
                    || tab.terminal.modes() != frozen.input.modes()
                {
                    return Err(refused(Error::BindingMismatch));
                }
                Some((frozen, tab))
            }
            (Ok(_), None) | (Err(_), Some(_)) => return Err(refused(Error::BindingMismatch)),
            (Err(_), None) => None,
        };
        if !reservation.matches_attachment(
            current.controller_instance,
            &current.attachment,
            current.attachment_generation,
            &guard,
        ) {
            return Err(refused(Error::BindingMismatch));
        }
        let broker = self
            .broker
            .as_mut()
            .ok_or_else(|| refused(Error::Unavailable))?;
        let commit = |broker: &mut Broker,
                      observation,
                      workspace: Option<(&WorkspaceRevision, u64)>| {
            reservation.commit_with(|bytes| {
                if let Some((workspace, revision)) = workspace {
                    if workspace.current() != Some(revision) {
                        broker.invalidate_presentation_monitoring();
                        return Err(Error::Revoked);
                    }
                }
                if let Some((frozen, tab)) = frozen {
                    if tab.terminal.modes() != frozen.input.modes() || bytes != frozen.input.bytes()
                    {
                        return Err(Error::BindingMismatch);
                    }
                }
                let exact_bytes = u32::try_from(bytes.len()).map_err(|_| Error::InvalidRequest)?;
                let digest = Digest(Sha256::digest(bytes).into());
                match observation {
                    Some(observation) => broker.commit_presented_control(
                        principal,
                        request,
                        digest,
                        exact_bytes,
                        observation,
                        Instant::now(),
                    ),
                    None => broker.commit_control(
                        principal,
                        request,
                        digest,
                        exact_bytes,
                        Instant::now(),
                    ),
                }
            })
        };
        let Some(barrier) = barrier else {
            return commit(broker, None, None);
        };
        let Some(workspace) = self.presentation_workspace.clone() else {
            broker.invalidate_presentation_monitoring();
            return Err(refused(Error::Unavailable));
        };
        let Some(workspace_revision) = workspace.current() else {
            broker.invalidate_presentation_monitoring();
            return Err(refused(Error::Unavailable));
        };
        let mut admission = None;
        let outcome =
            barrier.with_observation(broker, workspace_revision, |broker, observation| {
                admission = Some(commit(
                    broker,
                    Some(observation),
                    Some((&workspace, workspace_revision)),
                ));
            });
        match (outcome, admission) {
            (Ok(()), Some(result)) => result,
            (Ok(()), None) => {
                broker.invalidate_presentation_monitoring();
                Err(refused(Error::Unavailable))
            }
            (Err(error), admission) => {
                broker.invalidate_presentation_monitoring();
                match admission {
                    Some(Ok(())) | Some(Err(CommitAutomationInputError::LostAfterCommit)) => {
                        Err(CommitAutomationInputError::LostAfterCommit)
                    }
                    Some(Err(error)) => Err(error),
                    None => Err(refused(error)),
                }
            }
        }
    }
}
