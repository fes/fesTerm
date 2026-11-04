use crate::types::*;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

#[path = "presentation.rs"]
mod presentation;
pub use presentation::{
    ConfirmedPresentation, NativeControllerPresentationApproval, NativePresentationObservation,
    NativePresentationState, PresentationApprovalProposal, PresentationAttempt,
    PresentationMonitorId, PresentationRequirement, PresentationRevision, PresentationWindowId,
    MAX_PRESENTATION_ATTEMPTS_PER_GRANT, MAX_PRESENTATION_WAIT,
};
use presentation::{PresentationGrant, PresentationState};

#[derive(Clone, Copy, Default)]
struct Usage {
    operations: u32,
    bytes: u32,
}

struct Principal {
    grant: Option<Grant>,
    approvals_requested: u32,
    controller_registrations: BTreeMap<ControllerRegistrationId, ControllerRegistration>,
}

struct ControllerRegistration {
    local_id: Box<[u8]>,
    revision: u64,
    enrollment: Option<ControllerEnrollment>,
    presentation_review: Option<u64>,
    presentation_grant: Option<PresentationGrant>,
}

#[derive(Clone, Copy)]
struct ControllerEnrollment {
    id: ControllerEnrollmentId,
    target: TargetRef,
}

struct Grant {
    epoch: u64,
    request: GrantRequest,
    handoffs: BTreeMap<TargetRef, BTreeSet<TargetRef>>,
    expires: Instant,
    usage: Usage,
}

/// Opaque host-only identity of one approved control grant. It is not a wire
/// credential and is not serializable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeControlGrant {
    instance: [u8; 16],
    epoch: u64,
}

struct PendingApproval {
    connection: ConnectionId,
    request: GrantRequest,
    expires: Instant,
    ownership: BTreeMap<TargetRef, (u64, Option<LeaseRef>)>,
}

struct Target {
    reference: TargetRef,
    active: bool,
    ownership_epoch: u64,
    writer: Option<LeaseRef>,
}

struct Run {
    connection: ConnectionId,
    grant_epoch: u64,
    parent: Option<RunId>,
    spec: RunSpec,
    frozen_sources: BTreeSet<TargetRef>,
    frozen_controls: BTreeSet<TargetRef>,
    usage: Usage,
    status: RunStatus,
    started_workflow: Option<(StartKey, RunId)>,
}

#[derive(Clone, Copy)]
struct Request {
    run: RunId,
    operation: Operation,
    committed: bool,
}

struct StartReceipt {
    grant_epoch: u64,
    spec: RunSpec,
    run: RunId,
}

/// Pure state kernel; no transport, UI, engine, terminal snapshots or OS I/O.
///
/// Keep this value exclusively in the application-owned controller. Methods
/// accepting native decisions MUST NOT be projected onto a worker/wire API.
pub struct Broker {
    instance: [u8; 16],
    sequence: u64,
    limits: Limits,
    global: Usage,
    approvals_requested: u32,
    principals: BTreeMap<ConnectionId, Principal>,
    pending: BTreeMap<ApprovalId, PendingApproval>,
    targets: BTreeMap<u64, Target>,
    forgotten_generation: Option<u64>,
    runs: BTreeMap<RunId, Run>,
    requests: BTreeMap<RequestId, Request>,
    starts: BTreeMap<(ConnectionId, StartKey), StartReceipt>,
    presentation: PresentationState,
}

impl Broker {
    /// The application supplies a fresh unpredictable instance nonce at startup.
    /// Never reuse it across broker instances or accept it from a client.
    pub fn new(application_instance: [u8; 16]) -> Self {
        Self::with_limits(application_instance, Limits::default())
    }

    /// Requires the same fresh instance nonce as `new`. Limits may only lower
    /// the fixed qualification ceilings.
    pub fn with_limits(application_instance: [u8; 16], limits: Limits) -> Self {
        let ceiling = Limits::default();
        Self {
            instance: application_instance,
            sequence: 0,
            limits: Limits {
                connections: limits.connections.min(ceiling.connections),
                targets: limits.targets.min(ceiling.targets),
                controller_registrations: limits
                    .controller_registrations
                    .min(ceiling.controller_registrations),
                controller_registrations_per_connection: limits
                    .controller_registrations_per_connection
                    .min(ceiling.controller_registrations_per_connection),
                retained_runs: limits.retained_runs.min(ceiling.retained_runs),
                global_operations: limits.global_operations.min(ceiling.global_operations),
                global_disclosed_bytes: limits
                    .global_disclosed_bytes
                    .min(ceiling.global_disclosed_bytes),
            },
            global: Usage::default(),
            approvals_requested: 0,
            principals: BTreeMap::new(),
            pending: BTreeMap::new(),
            targets: BTreeMap::new(),
            forgotten_generation: None,
            runs: BTreeMap::new(),
            requests: BTreeMap::new(),
            starts: BTreeMap::new(),
            presentation: PresentationState::default(),
        }
    }

    fn mint(&mut self) -> Result<u64> {
        self.sequence = self.sequence.checked_add(1).ok_or(Error::QuotaExceeded)?;
        Ok(self.sequence)
    }

    /// Called only after the application acceptor authenticates a fresh channel.
    /// The returned channel identity has no scopes, even on the same OS account.
    pub fn accept_connection(&mut self) -> Result<ConnectionId> {
        if self.principals.len() >= self.limits.connections {
            return Err(Error::QuotaExceeded);
        }
        let connection = ConnectionId {
            instance: self.instance,
            sequence: self.mint()?,
        };
        self.principals.insert(
            connection,
            Principal {
                grant: None,
                approvals_requested: 0,
                controller_registrations: BTreeMap::new(),
            },
        );
        Ok(connection)
    }

    pub fn disconnect(&mut self, connection: ConnectionId) {
        self.revoke(connection);
        self.principals.remove(&connection);
        self.runs.retain(|_, run| run.connection != connection);
        self.starts.retain(|(owner, _), _| *owner != connection);
    }

    /// Record an untrusted, opaque local registration on this exact connection.
    /// This does not select a target, request a dialog, or grant any authority.
    /// Repeating the same bytes on this connection returns the existing ID.
    pub fn register_controller_session(
        &mut self,
        connection: ConnectionId,
        local_id: &[u8],
    ) -> Result<ControllerRegistrationId> {
        let principal = self
            .principals
            .get(&connection)
            .ok_or(Error::Unauthorized)?;
        if local_id.is_empty() || local_id.len() > MAX_CONTROLLER_REGISTRATION_BYTES {
            return Err(Error::InvalidRequest);
        }
        if let Some((id, _)) = principal
            .controller_registrations
            .iter()
            .find(|(_, registration)| registration.local_id.as_ref() == local_id)
        {
            return Ok(*id);
        }
        if principal.controller_registrations.len()
            >= self.limits.controller_registrations_per_connection
            || self
                .principals
                .values()
                .map(|principal| principal.controller_registrations.len())
                .sum::<usize>()
                >= self.limits.controller_registrations
        {
            return Err(Error::QuotaExceeded);
        }
        let id = ControllerRegistrationId {
            instance: self.instance,
            sequence: self.mint()?,
        };
        let principal = self
            .principals
            .get_mut(&connection)
            .ok_or(Error::Unauthorized)?;
        principal.controller_registrations.insert(
            id,
            ControllerRegistration {
                local_id: local_id.into(),
                revision: id.sequence,
                enrollment: None,
                presentation_review: None,
                presentation_grant: None,
            },
        );
        Ok(id)
    }

    /// Returns only the submitting connection's untrusted registration bytes.
    /// Native presentation must treat these bytes as potentially deceptive.
    pub fn controller_registration(
        &self,
        connection: ConnectionId,
        id: ControllerRegistrationId,
    ) -> Result<&[u8]> {
        self.principals
            .get(&connection)
            .and_then(|principal| principal.controller_registrations.get(&id))
            .map(|registration| registration.local_id.as_ref())
            .ok_or(Error::Unauthorized)
    }

    /// Attempt to freeze the latest native-selected candidate for review.
    /// Once the registration is verified, even a stale target supersedes older
    /// candidates without changing the existing association. A candidate is not
    /// an association and may not become an IPC-supplied decision.
    pub fn prepare_controller_enrollment(
        &mut self,
        connection: ConnectionId,
        registration: ControllerRegistrationId,
        target: TargetRef,
    ) -> Result<ControllerEnrollmentProposal> {
        self.principals
            .get(&connection)
            .and_then(|principal| principal.controller_registrations.get(&registration))
            .ok_or(Error::Unauthorized)?;
        let revision = self.mint()?;
        let record = self
            .principals
            .get_mut(&connection)
            .and_then(|principal| principal.controller_registrations.get_mut(&registration))
            .ok_or(Error::Unauthorized)?;
        record.revision = revision;
        self.target(target)?;
        Ok(ControllerEnrollmentProposal {
            connection,
            registration,
            target,
            revision,
        })
    }

    /// Apply one explicit native decision. A new reference is minted for every
    /// binding, including a rebind to the same target. Older decisions and
    /// references cannot redirect to the new association.
    pub fn approve_controller_enrollment(
        &mut self,
        connection: ConnectionId,
        decision: NativeControllerEnrollment,
    ) -> Result<ControllerEnrollmentId> {
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
        if record.revision != proposal.revision {
            return Err(Error::BindingMismatch);
        }
        self.target(proposal.target)?;
        let id = ControllerEnrollmentId {
            instance: self.instance,
            sequence: self.mint()?,
        };
        let record = self
            .principals
            .get_mut(&connection)
            .and_then(|principal| {
                principal
                    .controller_registrations
                    .get_mut(&proposal.registration)
            })
            .ok_or(Error::Unauthorized)?;
        record.revision = id.sequence;
        record.enrollment = Some(ControllerEnrollment {
            id,
            target: proposal.target,
        });
        record.presentation_review = None;
        record.presentation_grant = None;
        self.cancel_presentation_for_registration(connection, proposal.registration);
        Ok(id)
    }

    /// Resolve only the current association owned by this connection. The
    /// returned target is an internal reference, not a disclosure permission.
    pub fn resolve_controller_enrollment(
        &self,
        connection: ConnectionId,
        registration: ControllerRegistrationId,
        enrollment: ControllerEnrollmentId,
    ) -> Result<TargetRef> {
        let association = self
            .principals
            .get(&connection)
            .and_then(|principal| principal.controller_registrations.get(&registration))
            .and_then(|record| record.enrollment)
            .filter(|association| association.id == enrollment)
            .ok_or(Error::Unauthorized)?;
        self.target(association.target)?;
        Ok(association.target)
    }

    /// Forget one connection's registration and invalidate its association and
    /// any outstanding native-review candidates.
    pub fn unregister_controller_session(
        &mut self,
        connection: ConnectionId,
        registration: ControllerRegistrationId,
    ) -> Result<()> {
        self.principals
            .get_mut(&connection)
            .and_then(|principal| principal.controller_registrations.remove(&registration))
            .ok_or(Error::Unauthorized)?;
        self.cancel_presentation_for_registration(connection, registration);
        Ok(())
    }

    /// Attachment generations must strictly increase, including after
    /// close/reconnect. Closed IDs remain tombstoned until explicitly forgotten.
    pub fn attach_target(&mut self, session: u64, generation: u64) -> Result<TargetRef> {
        if let Some(old) = self.targets.get(&session) {
            if generation <= old.reference.generation {
                return Err(Error::StaleTarget);
            }
        } else if self
            .forgotten_generation
            .is_some_and(|forgotten| generation <= forgotten)
        {
            return Err(Error::StaleTarget);
        } else if self.targets.len() >= self.limits.targets {
            return Err(Error::QuotaExceeded);
        }
        let epoch = self.mint()?;
        if let Some(old) = self.targets.get(&session) {
            self.invalidate_target(old.reference);
        }
        let reference = TargetRef {
            instance: self.instance,
            session,
            generation,
        };
        self.targets.insert(
            session,
            Target {
                reference,
                active: true,
                ownership_epoch: epoch,
                writer: None,
            },
        );
        Ok(reference)
    }

    fn target(&self, reference: TargetRef) -> Result<&Target> {
        self.targets
            .get(&reference.session)
            .filter(|target| target.active && target.reference == reference)
            .ok_or(Error::StaleTarget)
    }

    pub fn close_target(&mut self, reference: TargetRef) -> Result<()> {
        self.target(reference)?;
        self.invalidate_target(reference);
        if let Some(target) = self.targets.get_mut(&reference.session) {
            target.active = false;
            target.writer = None;
        }
        Ok(())
    }

    /// Frees a closed target's slot without allowing an old reference to recur.
    ///
    /// After this is used, every registration for an ID without a retained
    /// tombstone must exceed the highest generation forgotten by this broker.
    /// Applications must therefore use globally increasing generations.
    pub fn forget_closed_target(&mut self, reference: TargetRef) -> Result<()> {
        let target = self
            .targets
            .get(&reference.session)
            .filter(|target| !target.active && target.reference == reference)
            .ok_or(Error::StaleTarget)?;
        let generation = target.reference.generation;
        self.forgotten_generation = Some(
            self.forgotten_generation
                .map_or(generation, |forgotten| forgotten.max(generation)),
        );
        self.targets.remove(&reference.session);
        Ok(())
    }

    fn invalidate_target(&mut self, target: TargetRef) {
        self.cancel_presentation_for_target(target);
        for principal in self.principals.values_mut() {
            principal
                .controller_registrations
                .retain(|_, registration| {
                    registration
                        .enrollment
                        .is_none_or(|association| association.target != target)
                });
        }
        let connections: Vec<_> = self
            .principals
            .iter()
            .filter_map(|(id, principal)| {
                principal
                    .grant
                    .as_ref()
                    .filter(|grant| grant.request.scope.targets().contains(&target))
                    .map(|_| *id)
            })
            .collect();
        for connection in connections {
            self.revoke(connection);
        }
        self.pending
            .retain(|_, pending| !pending.request.scope.targets().contains(&target));
    }

    fn validate_scope(&self, scope: &Scope) -> Result<()> {
        if scope.discover.len() > MAX_SCOPE_TARGETS
            || scope.observe.len() > MAX_SCOPE_TARGETS
            || scope.control.len() > MAX_SCOPE_TARGETS
            || scope.targets().len() > MAX_SCOPE_TARGETS
        {
            return Err(Error::InvalidRequest);
        }
        for target in scope.targets() {
            self.target(target)?;
        }
        Ok(())
    }

    fn validate_request(&self, request: &GrantRequest) -> Result<()> {
        if (request.bindings.is_empty() && request.direct_read_calls.is_empty())
            || request.bindings.len() > 16
            || request
                .bindings
                .iter()
                .any(|binding| binding.api_version != 1)
            || request
                .direct_read_calls
                .iter()
                .any(|kind| !matches!(kind, DirectKind::Discovery | DirectKind::Observation))
        {
            return Err(Error::InvalidRequest);
        }
        self.validate_scope(&request.scope)
    }

    /// Only an explicit paired-client/user invocation may be routed here by the
    /// application. This records a bounded proposal, not authority or a dialog.
    pub fn request_approval(
        &mut self,
        connection: ConnectionId,
        request: GrantRequest,
        now: Instant,
    ) -> Result<ApprovalId> {
        self.poll_expiry(now);
        self.validate_request(&request)?;
        let principal = self
            .principals
            .get(&connection)
            .ok_or(Error::Unauthorized)?;
        if self.pending.len() >= MAX_PENDING_APPROVALS
            || self
                .pending
                .values()
                .filter(|pending| pending.connection == connection)
                .count()
                >= 2
            || principal.approvals_requested >= 8
            || self.approvals_requested >= 128
        {
            return Err(Error::QuotaExceeded);
        }
        let expires = now
            .checked_add(MAX_GRANT_LIFETIME)
            .ok_or(Error::InvalidRequest)?;
        let ownership = request
            .scope
            .control
            .iter()
            .map(|reference| {
                let target = self.target(*reference)?;
                Ok((*reference, (target.ownership_epoch, target.writer)))
            })
            .collect::<Result<_>>()?;
        let id = ApprovalId {
            instance: self.instance,
            sequence: self.mint()?,
        };
        self.pending.insert(
            id,
            PendingApproval {
                connection,
                request,
                expires,
                ownership,
            },
        );
        self.principals
            .get_mut(&connection)
            .ok_or(Error::Unauthorized)?
            .approvals_requested += 1;
        self.approvals_requested += 1;
        Ok(id)
    }

    /// Whether this connection's proposal is still recorded and unexpired.
    /// Ownership and scope must still be checked when a native decision is applied.
    pub fn pending_approval_is_active(
        &self,
        connection: ConnectionId,
        id: ApprovalId,
        now: Instant,
    ) -> bool {
        self.pending
            .get(&id)
            .is_some_and(|pending| pending.connection == connection && now < pending.expires)
    }

    pub fn reject_approval(&mut self, connection: ConnectionId, id: ApprovalId) -> Result<()> {
        if self
            .pending
            .get(&id)
            .is_none_or(|pending| pending.connection != connection)
        {
            return Err(Error::Unauthorized);
        }
        self.pending.remove(&id);
        Ok(())
    }

    /// Native-only decision, bound to the exact pending channel/proposal.
    /// Reapproval revokes all old runs; only this explicit native decision resets
    /// that principal's shared counters. Global counters never reset.
    pub fn approve(
        &mut self,
        connection: ConnectionId,
        id: ApprovalId,
        approval: NativeApproval,
        now: Instant,
    ) -> Result<()> {
        self.poll_expiry(now);
        let pending = self.pending.get(&id).ok_or(Error::Unauthorized)?;
        if pending.connection != connection
            || !approval.request.scope.subset_of(&pending.request.scope)
            || !approval
                .request
                .bindings
                .is_subset(&pending.request.bindings)
            || !approval
                .request
                .direct_read_calls
                .is_subset(&pending.request.direct_read_calls)
        {
            return Err(Error::Unauthorized);
        }
        self.validate_request(&approval.request)?;
        for reference in &approval.request.scope.control {
            let target = self.target(*reference)?;
            if pending.ownership.get(reference) != Some(&(target.ownership_epoch, target.writer)) {
                return Err(Error::Revoked);
            }
        }
        if approval.lifetime.is_zero() || approval.lifetime > MAX_GRANT_LIFETIME {
            return Err(Error::InvalidRequest);
        }
        Self::validate_closure(
            &approval.request.scope.sources(),
            &approval.request.scope.control,
            &approval.handoffs,
        )?;
        let expires = now
            .checked_add(approval.lifetime)
            .ok_or(Error::InvalidRequest)?;
        let epoch = self.mint()?;
        self.revoke(connection);
        let principal = self
            .principals
            .get_mut(&connection)
            .ok_or(Error::Unauthorized)?;
        principal.grant = Some(Grant {
            epoch,
            request: approval.request,
            handoffs: approval.handoffs,
            expires,
            usage: Usage::default(),
        });
        Ok(())
    }

    fn validate_closure(
        sources: &BTreeSet<TargetRef>,
        controls: &BTreeSet<TargetRef>,
        handoffs: &BTreeMap<TargetRef, BTreeSet<TargetRef>>,
    ) -> Result<()> {
        for target in controls {
            if handoffs
                .get(target)
                .is_none_or(|approved| !sources.is_subset(approved))
            {
                return Err(Error::Unauthorized);
            }
        }
        Ok(())
    }

    fn grant(&self, connection: ConnectionId, now: Instant) -> Result<&Grant> {
        let principal = self
            .principals
            .get(&connection)
            .ok_or(Error::Unauthorized)?;
        let grant = principal.grant.as_ref().ok_or(Error::Revoked)?;
        if now >= grant.expires {
            return Err(Error::Expired);
        }
        Ok(grant)
    }

    /// Identifies the current accepted native handoff for this principal and
    /// exact target. This identifies a grant; it does not authorize an effect.
    pub fn native_control_grant(
        &self,
        connection: ConnectionId,
        target: TargetRef,
        now: Instant,
    ) -> Result<NativeControlGrant> {
        self.target(target)?;
        let grant = self.grant(connection, now)?;
        if !grant.request.scope.control.contains(&target) || !grant.handoffs.contains_key(&target) {
            return Err(Error::Unauthorized);
        }
        Ok(NativeControlGrant {
            instance: self.instance,
            epoch: grant.epoch,
        })
    }

    pub fn revoke(&mut self, connection: ConnectionId) {
        if let Some(principal) = self.principals.get_mut(&connection) {
            principal.grant = None;
            for registration in principal.controller_registrations.values_mut() {
                registration.presentation_review = None;
                registration.presentation_grant = None;
            }
        }
        self.cancel_presentation_for_connection(connection);
        self.pending
            .retain(|_, pending| pending.connection != connection);
        let runs: Vec<_> = self
            .runs
            .iter()
            .filter(|(_, run)| {
                run.connection == connection && run.status.state == RunState::Running
            })
            .map(|(id, _)| *id)
            .collect();
        for id in runs {
            self.cut(id, RunState::Interrupted, Some(Error::Revoked));
        }
    }

    /// Lock/suspend/privacy loss is a global revocation, never a pause/resume.
    pub fn revoke_all(&mut self) {
        let connections: Vec<_> = self.principals.keys().copied().collect();
        for connection in connections {
            self.revoke(connection);
        }
        for principal in self.principals.values_mut() {
            principal.controller_registrations.clear();
        }
        self.stop_all_presentation_monitoring();
    }

    /// Call from the application's deadline timer to wake/reap external waiters
    /// and workers. Every admission/delivery also checks time independently.
    pub fn poll_expiry(&mut self, now: Instant) {
        self.expire_presentations(now);
        self.pending.retain(|_, pending| now < pending.expires);
        let expired: Vec<_> = self
            .runs
            .iter()
            .filter(|(_, run)| run.status.state == RunState::Running && now >= run.spec.deadline)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            self.cut(id, RunState::TimedOut, Some(Error::Expired));
        }
        let connections: Vec<_> = self
            .principals
            .iter()
            .filter_map(|(id, principal)| {
                principal
                    .grant
                    .as_ref()
                    .filter(|grant| now >= grant.expires)
                    .map(|_| *id)
            })
            .collect();
        for connection in connections {
            self.revoke(connection);
        }
    }

    fn check_run(&self, connection: ConnectionId, id: RunId, now: Instant) -> Result<&Run> {
        let run = self.runs.get(&id).ok_or(Error::Unauthorized)?;
        if run.connection != connection {
            return Err(Error::Unauthorized);
        }
        let grant = self.grant(connection, now)?;
        if run.grant_epoch != grant.epoch {
            return Err(Error::Revoked);
        }
        if now >= run.spec.deadline {
            return Err(Error::Expired);
        }
        if run.status.state != RunState::Running {
            return Err(Error::Closed);
        }
        for target in run.frozen_sources.union(&run.frozen_controls) {
            self.target(*target)?;
        }
        if let Some(parent) = run.parent {
            self.check_run(connection, parent, now)?;
        }
        Self::validate_closure(&run.frozen_sources, &run.frozen_controls, &grant.handoffs)?;
        Ok(run)
    }

    fn lineage(&self, mut id: RunId) -> Vec<RunId> {
        let mut ids = Vec::new();
        while let Some(run) = self.runs.get(&id) {
            ids.push(id);
            match run.parent {
                Some(parent) => id = parent,
                None => break,
            }
        }
        ids
    }

    fn charge(
        &mut self,
        connection: ConnectionId,
        lineage: &[RunId],
        operations: u32,
        bytes: u32,
        now: Instant,
    ) -> Result<()> {
        let fits = |usage: Usage, budget: Budget| {
            usage
                .operations
                .checked_add(operations)
                .is_some_and(|value| value <= budget.operations)
                && usage
                    .bytes
                    .checked_add(bytes)
                    .is_some_and(|value| value <= budget.disclosed_bytes)
        };
        if !fits(
            self.global,
            Budget {
                operations: self.limits.global_operations,
                disclosed_bytes: self.limits.global_disclosed_bytes,
            },
        ) || !fits(self.grant(connection, now)?.usage, Budget::default())
        {
            return Err(Error::QuotaExceeded);
        }
        for id in lineage {
            let run = self.check_run(connection, *id, now)?;
            if !fits(run.usage, run.spec.budget) {
                return Err(Error::QuotaExceeded);
            }
        }
        self.global.operations += operations;
        self.global.bytes += bytes;
        let grant = self
            .principals
            .get_mut(&connection)
            .and_then(|principal| principal.grant.as_mut())
            .ok_or(Error::Revoked)?;
        grant.usage.operations += operations;
        grant.usage.bytes += bytes;
        for id in lineage {
            let run = self.runs.get_mut(id).ok_or(Error::Unauthorized)?;
            run.usage.operations += operations;
            run.usage.bytes += bytes;
        }
        Ok(())
    }

    /// Atomic run admission under a live native grant. With parent=None the run
    /// is connection-owned (including an asynchronous MCP start); it is NOT a
    /// child of the short start request. JS children MUST pass their actual parent.
    ///
    /// No worker is started here. External launch failure must call cancel_run;
    /// no budget refund or automatic restart is authorized.
    pub fn admit_run(
        &mut self,
        connection: ConnectionId,
        parent: Option<RunId>,
        spec: RunSpec,
        now: Instant,
    ) -> Result<RunId> {
        self.poll_expiry(now);
        self.validate_scope(&spec.scope)?;
        if spec.binding.api_version != 1 {
            return Err(Error::InvalidRequest);
        }
        let grant = self.grant(connection, now)?;
        let binding_approved = grant.request.bindings.contains(&spec.binding)
            || matches!(
                spec.binding.invocation,
                Invocation::Direct { operation }
                    if grant.request.direct_read_calls.contains(&operation)
            );
        if !spec.scope.subset_of(&grant.request.scope) || !binding_approved {
            return Err(Error::Unauthorized);
        }
        if spec.deadline <= now
            || spec.deadline > grant.expires
            || spec.budget.operations == 0
            || spec.budget.operations > Budget::default().operations
            || spec.budget.disclosed_bytes > Budget::default().disclosed_bytes
        {
            return Err(Error::InvalidRequest);
        }
        let workflow = matches!(spec.binding.invocation, Invocation::Workflow { .. });
        if workflow && !grant.request.scope.invoke_workflows {
            return Err(Error::Unauthorized);
        }
        let active = self
            .runs
            .values()
            .filter(|run| run.status.state == RunState::Running);
        let same_kind = |run: &&Run| {
            matches!(run.spec.binding.invocation, Invocation::Workflow { .. }) == workflow
        };
        if self.runs.len() >= self.limits.retained_runs
            || active
                .clone()
                .filter(same_kind)
                .filter(|run| run.connection == connection)
                .count()
                >= if workflow { 2 } else { 4 }
            || (workflow && active.filter(same_kind).count() >= 4)
        {
            return Err(Error::QuotaExceeded);
        }
        let grant_epoch = grant.epoch;
        let mut frozen_sources = spec.scope.sources();
        let mut frozen_controls = spec.scope.control.clone();
        let lineage = if let Some(parent) = parent {
            let parent_run = self.check_run(connection, parent, now)?;
            if !workflow
                || !matches!(
                    parent_run.spec.binding.invocation,
                    Invocation::Workflow { .. }
                )
                || !parent_run.spec.scope.invoke_workflows
                || !spec.scope.subset_of(&parent_run.spec.scope)
                || spec.deadline > parent_run.spec.deadline
                || spec.budget.operations > parent_run.spec.budget.operations
                || spec.budget.disclosed_bytes > parent_run.spec.budget.disclosed_bytes
            {
                return Err(Error::Unauthorized);
            }
            let lineage = self.lineage(parent);
            if lineage.len() > 2 {
                return Err(Error::QuotaExceeded);
            }
            frozen_sources.extend(&parent_run.frozen_sources);
            frozen_controls.extend(&parent_run.frozen_controls);
            lineage
        } else {
            Vec::new()
        };
        Self::validate_closure(&frozen_sources, &frozen_controls, &grant.handoffs)?;
        let id = RunId {
            instance: self.instance,
            sequence: self.mint()?,
        };
        self.charge(connection, &lineage, 1, 0, now)?;
        self.runs.insert(
            id,
            Run {
                connection,
                grant_epoch,
                parent,
                spec,
                frozen_sources,
                frozen_controls,
                usage: Usage {
                    operations: 1,
                    bytes: 0,
                },
                status: RunStatus {
                    state: RunState::Running,
                    failure: None,
                    admitted_effects: 0,
                    execution_outcome_unknown: false,
                },
                started_workflow: None,
            },
        );
        Ok(id)
    }

    /// Connection-owned asynchronous workflow admission, not a JS child.
    /// The caller must pass a live, bounded direct WorkflowStart context whose
    /// validated parameters describe this immutable spec. Identical retries
    /// return the original run; mismatches and old grant epochs refuse.
    ///
    /// Receipts never evict during a connection lifetime (bounded at 128).
    /// Unknown receipt/broker restart MUST NOT trigger automatic retries.
    pub fn start_workflow(
        &mut self,
        connection: ConnectionId,
        context: RunId,
        key: StartKey,
        spec: RunSpec,
        now: Instant,
    ) -> Result<RunId> {
        self.poll_expiry(now);
        let caller = self.check_run(connection, context, now)?;
        if caller.spec.binding.invocation
            != (Invocation::Direct {
                operation: DirectKind::WorkflowStart,
            })
            || !caller.spec.scope.invoke_workflows
            || !spec.scope.subset_of(&caller.spec.scope)
            || !matches!(spec.binding.invocation, Invocation::Workflow { .. })
        {
            return Err(Error::Unauthorized);
        }
        let epoch = caller.grant_epoch;
        if caller
            .started_workflow
            .is_some_and(|(previous, _)| previous != key)
        {
            return Err(Error::BindingMismatch);
        }
        let inherited_sources = caller.frozen_sources.clone();
        let inherited_controls = caller.frozen_controls.clone();
        if let Some(receipt) = self.starts.get(&(connection, key)) {
            if receipt.grant_epoch != epoch {
                return Err(Error::Revoked);
            }
            if receipt.spec != spec {
                return Err(Error::BindingMismatch);
            }
            let run = receipt.run;
            self.charge(connection, &self.lineage(context), 1, 0, now)?;
            self.runs
                .get_mut(&context)
                .ok_or(Error::Closed)?
                .started_workflow = Some((key, run));
            return Ok(run);
        }
        if self.starts.len() >= 128 {
            return Err(Error::QuotaExceeded);
        }
        self.charge(connection, &self.lineage(context), 1, 0, now)?;
        let run = self.admit_run(connection, None, spec.clone(), now)?;
        let admitted = self.runs.get_mut(&run).ok_or(Error::Closed)?;
        admitted.frozen_sources.extend(inherited_sources);
        admitted.frozen_controls.extend(inherited_controls);
        self.runs
            .get_mut(&context)
            .ok_or(Error::Closed)?
            .started_workflow = Some((key, run));
        self.starts.insert(
            (connection, key),
            StartReceipt {
                grant_epoch: epoch,
                spec,
                run,
            },
        );
        Ok(run)
    }

    /// A protocol cancellation of the exact admitted start is stronger than
    /// normal completion of its short context. The receipt remains permanent;
    /// an identical retry cannot create a replacement.
    pub fn cancel_start(
        &mut self,
        connection: ConnectionId,
        key: StartKey,
        now: Instant,
    ) -> Result<()> {
        self.poll_expiry(now);
        let receipt = self
            .starts
            .get(&(connection, key))
            .ok_or(Error::Unauthorized)?;
        if self.grant(connection, now)?.epoch != receipt.grant_epoch {
            return Err(Error::Revoked);
        }
        self.cut(receipt.run, RunState::Cancelled, None);
        Ok(())
    }

    /// Caller-owned, fixed-schema status only. No output, exception messages or
    /// parameters are retained. Even status retrieval consumes shared budgets.
    pub fn status(
        &mut self,
        connection: ConnectionId,
        id: RunId,
        now: Instant,
    ) -> Result<RunStatus> {
        self.poll_expiry(now);
        let run = self.runs.get(&id).ok_or(Error::Unauthorized)?;
        if run.connection != connection {
            return Err(Error::Unauthorized);
        }
        let grant = self.grant(connection, now)?;
        if run.grant_epoch != grant.epoch || !grant.request.scope.return_results {
            return Err(Error::Unauthorized);
        }
        let status = run.status;
        self.charge(connection, &[], 1, 256, now)?;
        Ok(status)
    }

    /// Forget only settled leaves. Shared grant/global counters are unchanged,
    /// so allocating many short contexts cannot replenish an allowance.
    pub fn forget_run(&mut self, connection: ConnectionId, id: RunId) -> Result<()> {
        let run = self.runs.get(&id).ok_or(Error::Unauthorized)?;
        if run.connection != connection {
            return Err(Error::Unauthorized);
        }
        if run.status.state == RunState::Running
            || self.runs.values().any(|child| child.parent == Some(id))
            || self.starts.values().any(|receipt| receipt.run == id)
        {
            return Err(Error::Busy);
        }
        self.runs.remove(&id);
        Ok(())
    }

    pub fn prepare_lease(
        &self,
        connection: ConnectionId,
        id: RunId,
        target: TargetRef,
        now: Instant,
    ) -> Result<LeaseTransition> {
        let run = self.check_run(connection, id, now)?;
        if !matches!(run.spec.binding.invocation, Invocation::Workflow { .. })
            || !run.spec.scope.control.contains(&target)
        {
            return Err(Error::Unauthorized);
        }
        let target_state = self.target(target)?;
        if target_state.writer.is_some() {
            return Err(Error::Busy);
        }
        Ok(LeaseTransition {
            connection,
            run: id,
            target,
            observed_epoch: target_state.ownership_epoch,
        })
    }

    /// Controller-only commit: first serialize human input, reject unsafe local
    /// fragments, invalidate clipboard/IME/paste reservations and reserve the
    /// input barrier. Call this without an intervening controller event, then
    /// install the returned ownership epoch in that controller. If any of those
    /// preconditions fail, DROP the transition; do not call this method.
    pub fn commit_lease(&mut self, transition: LeaseTransition, now: Instant) -> Result<LeaseRef> {
        self.check_run(transition.connection, transition.run, now)?;
        let target = self.target(transition.target)?;
        if target.ownership_epoch != transition.observed_epoch || target.writer.is_some() {
            return Err(Error::Busy);
        }
        let lease = LeaseRef {
            target: transition.target,
            run: transition.run,
            epoch: self.mint()?,
        };
        let target = self
            .targets
            .get_mut(&lease.target.session)
            .ok_or(Error::StaleTarget)?;
        target.ownership_epoch = lease.epoch;
        target.writer = Some(lease);
        Ok(lease)
    }

    /// Must precede admission of any human bytes/reservation. Observation-only
    /// grants survive; every grant authorizing control of this target is ended,
    /// even if its prepared acquisition has not committed yet.
    pub fn human_takeover(&mut self, reference: TargetRef) -> Result<()> {
        self.target(reference)?;
        self.cancel_presentation_for_target(reference);
        let epoch = self.mint()?;
        let connections: Vec<_> = self
            .principals
            .iter()
            .filter_map(|(id, principal)| {
                principal
                    .grant
                    .as_ref()
                    .filter(|grant| grant.request.scope.control.contains(&reference))
                    .map(|_| *id)
            })
            .collect();
        for connection in connections {
            self.revoke(connection);
        }
        let target = self
            .targets
            .get_mut(&reference.session)
            .ok_or(Error::StaleTarget)?;
        target.ownership_epoch = epoch;
        target.writer = None;
        Ok(())
    }

    fn check_operation(
        &self,
        id: RunId,
        run: &Run,
        operation: Operation,
        now: Instant,
    ) -> Result<()> {
        let kind_allowed = match run.spec.binding.invocation {
            Invocation::Workflow { .. } => true,
            Invocation::Direct { operation: kind } => matches!(
                (kind, operation),
                (DirectKind::Discovery, Operation::Discover { .. })
                    | (DirectKind::Observation, Operation::Observe { .. })
            ),
        };
        if !kind_allowed {
            return Err(Error::Unauthorized);
        }
        match operation {
            Operation::Discover { max_bytes } => {
                if max_bytes == 0 || max_bytes > MAX_PAYLOAD_BYTES {
                    return Err(Error::InvalidRequest);
                }
            }
            Operation::Observe { target, max_bytes } => {
                if !run.spec.scope.observe.contains(&target) {
                    return Err(Error::Unauthorized);
                }
                if max_bytes == 0 || max_bytes > MAX_OBSERVATION_BYTES {
                    return Err(Error::InvalidRequest);
                }
                self.target(target)?;
            }
            Operation::Control {
                lease,
                input: _,
                bytes,
                presentation,
            } => {
                if lease.run != id || !run.spec.scope.control.contains(&lease.target) {
                    return Err(Error::Unauthorized);
                }
                if bytes == 0 || bytes > MAX_INPUT_BYTES {
                    return Err(Error::InvalidRequest);
                }
                let target = self.target(lease.target)?;
                if target.writer != Some(lease) || target.ownership_epoch != lease.epoch {
                    return Err(Error::Revoked);
                }
                if let Some(requirement) = presentation {
                    self.check_presentation(run.connection, requirement, lease.target, now)?;
                }
            }
            Operation::Result { max_bytes } => {
                if !run.spec.scope.return_results {
                    return Err(Error::Unauthorized);
                }
                if max_bytes == 0 || max_bytes > MAX_PAYLOAD_BYTES {
                    return Err(Error::InvalidRequest);
                }
            }
        }
        Ok(())
    }

    /// Reserves cumulative maximum disclosure, never refunded on abort. No
    /// effects or data are admitted here. At most 16 calls can remain unsettled.
    pub fn prepare_request(
        &mut self,
        connection: ConnectionId,
        id: RunId,
        operation: Operation,
        now: Instant,
    ) -> Result<RequestId> {
        self.poll_expiry(now);
        let run = self.check_run(connection, id, now)?;
        self.check_operation(id, run, operation, now)?;
        if self
            .requests
            .values()
            .filter(|request| request.run == id)
            .count()
            >= 16
        {
            return Err(Error::QuotaExceeded);
        }
        let request = RequestId {
            instance: self.instance,
            sequence: self.mint()?,
        };
        self.charge(
            connection,
            &self.lineage(id),
            1,
            operation.reserved_bytes(),
            now,
        )?;
        self.requests.insert(
            request,
            Request {
                run: id,
                operation,
                committed: false,
            },
        );
        Ok(request)
    }

    fn check_request(
        &self,
        connection: ConnectionId,
        id: RequestId,
        now: Instant,
    ) -> Result<(&Request, &Run)> {
        let request = self.requests.get(&id).ok_or(Error::Closed)?;
        let run = self.check_run(connection, request.run, now)?;
        self.check_operation(request.run, run, request.operation, now)?;
        Ok((request, run))
    }

    /// Only these exact references may be projected into metadata by the app.
    /// Observe-only targets do not implicitly gain session discovery.
    pub fn discovery_targets(
        &self,
        connection: ConnectionId,
        id: RequestId,
        now: Instant,
    ) -> Result<Vec<TargetRef>> {
        let (request, run) = self.check_request(connection, id, now)?;
        if !matches!(request.operation, Operation::Discover { .. }) || request.committed {
            return Err(Error::InvalidRequest);
        }
        Ok(run.spec.scope.discover.iter().copied().collect())
    }

    /// Immediately before releasing the already bounded response on the same
    /// authenticated channel. The app must use ONLY the requested snapshot or
    /// discovery projection and must not cache this decision for later sends.
    pub fn commit_disclosure(
        &mut self,
        connection: ConnectionId,
        id: RequestId,
        encoded_bytes: u32,
        now: Instant,
    ) -> Result<()> {
        let (request, _) = self.check_request(connection, id, now)?;
        if request.committed
            || matches!(request.operation, Operation::Control { .. })
            || encoded_bytes > request.operation.reserved_bytes()
        {
            return Err(Error::InvalidRequest);
        }
        self.requests.get_mut(&id).ok_or(Error::Closed)?.committed = true;
        Ok(())
    }

    /// At the serialized transport admission boundary: reserve an infallible
    /// bounded queue slot BEFORE this call, rehash exact encoded input, then
    /// immediately fill that slot without processing another controller event.
    /// Success means queue admission only, never shell completion. No retries.
    pub fn commit_control(
        &mut self,
        connection: ConnectionId,
        id: RequestId,
        exact_input: Digest,
        exact_bytes: u32,
        now: Instant,
    ) -> Result<()> {
        self.commit_control_inner(connection, id, exact_input, exact_bytes, None, now)
    }

    /// Resolve a prepared control request for host-side attachment and human
    /// input fencing. This is not a disclosure or an admission decision.
    pub fn prepared_control_target(
        &self,
        connection: ConnectionId,
        id: RequestId,
        now: Instant,
    ) -> Result<TargetRef> {
        let request = self.requests.get(&id).ok_or(Error::Closed)?;
        self.check_run(connection, request.run, now)?;
        match request.operation {
            Operation::Control { lease, .. } if !request.committed => Ok(lease.target),
            _ => Err(Error::InvalidRequest),
        }
    }

    /// Resolves the frozen lease and grant epoch of one prepared request for
    /// host-side handoff binding. Final admission must still revalidate both.
    pub fn prepared_control_binding(
        &self,
        connection: ConnectionId,
        id: RequestId,
        now: Instant,
    ) -> Result<(LeaseRef, NativeControlGrant)> {
        let request = self.requests.get(&id).ok_or(Error::Closed)?;
        let run = self.check_run(connection, request.run, now)?;
        let Operation::Control { lease, .. } = request.operation else {
            return Err(Error::InvalidRequest);
        };
        if request.committed || lease.run != request.run {
            return Err(Error::InvalidRequest);
        }
        let grant = self.native_control_grant(connection, lease.target, now)?;
        if grant.epoch != run.grant_epoch {
            return Err(Error::Revoked);
        }
        Ok((lease, grant))
    }

    /// Commits a request that froze a presentation requirement. The observation
    /// must come from a fresh qualified native barrier, under the same host
    /// admission boundary as the attachment, privacy and human-input fences.
    pub fn commit_presented_control(
        &mut self,
        connection: ConnectionId,
        id: RequestId,
        exact_input: Digest,
        exact_bytes: u32,
        observation: NativePresentationObservation,
        now: Instant,
    ) -> Result<()> {
        self.commit_control_inner(
            connection,
            id,
            exact_input,
            exact_bytes,
            Some(observation),
            now,
        )
    }

    fn commit_control_inner(
        &mut self,
        connection: ConnectionId,
        id: RequestId,
        exact_input: Digest,
        exact_bytes: u32,
        observation: Option<NativePresentationObservation>,
        now: Instant,
    ) -> Result<()> {
        let (request, _) = self.check_request(connection, id, now)?;
        if request.committed {
            return Err(Error::Closed);
        }
        let Operation::Control {
            lease,
            input,
            bytes,
            presentation,
        } = request.operation
        else {
            return Err(Error::InvalidRequest);
        };
        if exact_input != input || exact_bytes != bytes {
            return Err(Error::BindingMismatch);
        }
        let run = request.run;
        match (presentation, observation) {
            (None, None) => {}
            (Some(requirement), Some(observation)) => {
                self.revalidate_presentation(
                    connection,
                    requirement,
                    lease.target,
                    observation,
                    now,
                )?;
            }
            _ => return Err(Error::BindingMismatch),
        }
        self.requests.get_mut(&id).ok_or(Error::Closed)?.committed = true;
        for id in self.lineage(run) {
            let run = self.runs.get_mut(&id).ok_or(Error::Closed)?;
            run.status.admitted_effects += 1;
            run.status.execution_outcome_unknown = true;
        }
        Ok(())
    }

    /// Acknowledge host response delivery or abort an unadmitted request. Both
    /// consume the reserved budget. Aborting an admitted effect is NOT rollback.
    pub fn settle_request(&mut self, connection: ConnectionId, id: RequestId) -> Result<()> {
        let request = self.requests.get(&id).ok_or(Error::Closed)?;
        if self
            .runs
            .get(&request.run)
            .is_none_or(|run| run.connection != connection)
        {
            return Err(Error::Unauthorized);
        }
        self.requests.remove(&id);
        Ok(())
    }

    fn cut(&mut self, id: RunId, state: RunState, failure: Option<Error>) {
        let descendants: Vec<_> = self
            .runs
            .keys()
            .filter(|candidate| self.lineage(**candidate).contains(&id))
            .copied()
            .collect();
        let affected: BTreeSet<_> = descendants.iter().copied().collect();
        self.requests
            .retain(|_, request| !affected.contains(&request.run));
        for target in self.targets.values_mut() {
            if target
                .writer
                .is_some_and(|lease| affected.contains(&lease.run))
            {
                // Epochs are never reused; removing the lease already rejects
                // every old lease and pending request. Acquisition mints anew.
                target.writer = None;
            }
        }
        for child in descendants {
            if let Some(run) = self.runs.get_mut(&child) {
                if run.status.state == RunState::Running {
                    run.status.state = if child == id {
                        state
                    } else {
                        RunState::Cancelled
                    };
                    run.status.failure = failure;
                }
            }
        }
    }

    /// Completion cut wins over late host calls. The application must destroy
    /// external waits/workers after this synchronous closure, within its bounded
    /// cleanup watchdog; this kernel does not claim process cleanup.
    pub fn complete(
        &mut self,
        connection: ConnectionId,
        id: RunId,
        completion: Completion,
        now: Instant,
    ) -> Result<RunStatus> {
        self.poll_expiry(now);
        self.check_run(connection, id, now)?;
        let descendants: BTreeSet<_> = self
            .runs
            .keys()
            .filter(|candidate| self.lineage(**candidate).contains(&id))
            .copied()
            .collect();
        let outstanding = self
            .requests
            .values()
            .any(|request| descendants.contains(&request.run));
        let children = descendants.iter().any(|child| {
            *child != id
                && self
                    .runs
                    .get(child)
                    .is_some_and(|run| run.status.state == RunState::Running)
        });
        let failure = if outstanding {
            Some(Error::UnsettledHostCalls)
        } else if children {
            Some(Error::UnsettledChildRuns)
        } else {
            None
        };
        let state = if failure.is_some() || completion == Completion::Failed {
            RunState::Failed
        } else {
            RunState::Completed
        };
        self.cut(id, state, failure);
        Ok(self.runs.get(&id).ok_or(Error::Closed)?.status)
    }

    /// Atomic successful completion and final-result disclosure admission.
    /// The app first freezes/encodes the bounded result, prepares a Result
    /// request, then calls this at the actual response admission boundary.
    /// On Err it MUST discard the result; unresolved work closes the run failed.
    /// On Ok it must immediately admit this one response without an intervening
    /// controller event. Neither this status nor a request ID is a later permit.
    pub fn complete_with_result(
        &mut self,
        connection: ConnectionId,
        id: RunId,
        result: RequestId,
        encoded_bytes: u32,
        now: Instant,
    ) -> Result<RunStatus> {
        let (request, _) = self.check_request(connection, result, now)?;
        if request.run != id
            || request.committed
            || !matches!(request.operation, Operation::Result { .. })
            || encoded_bytes > request.operation.reserved_bytes()
        {
            return Err(Error::InvalidRequest);
        }
        self.requests.remove(&result);
        let status = self.complete(connection, id, Completion::Returned, now)?;
        if status.state != RunState::Completed {
            return Err(status.failure.unwrap_or(Error::Closed));
        }
        Ok(status)
    }

    pub fn cancel_run(&mut self, connection: ConnectionId, id: RunId, now: Instant) -> Result<()> {
        self.poll_expiry(now);
        let run = self.runs.get(&id).ok_or(Error::Unauthorized)?;
        if run.connection != connection {
            return Err(Error::Unauthorized);
        }
        if run.grant_epoch != self.grant(connection, now)?.epoch {
            return Err(Error::Revoked);
        }
        let started_workflow = run.started_workflow;
        if let Some((_, workflow)) = started_workflow {
            // Delayed explicit cancellation still wins after a normal response
            // closed the short context. Normal completion never does this.
            self.cut(workflow, RunState::Cancelled, None);
        } else {
            self.check_run(connection, id, now)?;
        }
        self.cut(id, RunState::Cancelled, None);
        Ok(())
    }
}
