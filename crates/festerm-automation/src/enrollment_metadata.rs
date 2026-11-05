use super::*;
use std::time::Duration;

/// Exact host-only identity and source of an existing enrollment grant.
/// This identifies a review subject; it does not authorize disclosure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct NativeEnrollmentMetadataGrant {
    pub(super) connection: ConnectionId,
    pub(super) registration: ControllerRegistrationId,
    pub(super) enrollment: ControllerEnrollmentId,
    pub(super) target: TargetRef,
    pub(super) epoch: u64,
    pub(super) expires: Instant,
}

impl NativeEnrollmentMetadataGrant {
    pub fn registration(self) -> ControllerRegistrationId {
        self.registration
    }

    pub fn enrollment(self) -> ControllerEnrollmentId {
        self.enrollment
    }

    pub fn target(self) -> TargetRef {
        self.target
    }

    pub fn expires(self) -> Instant {
        self.expires
    }
}

/// Frozen native review for one exact ordinary approval, not a wire credential.
///
/// ```compile_fail
/// use festerm_automation::NativeOrdinaryEnrollmentReview;
/// let _ = serde_json::from_str::<NativeOrdinaryEnrollmentReview>("{}");
/// ```
#[derive(Clone)]
pub struct NativeOrdinaryEnrollmentReview {
    pub(super) connection: ConnectionId,
    pub(super) id: ApprovalId,
    pub(super) revisions: GrantRevisions,
    pub(super) ordinary_epoch: Option<u64>,
    pub(super) grants: Vec<NativeEnrollmentMetadataGrant>,
    pub(super) sinks: BTreeSet<TargetRef>,
}

impl NativeOrdinaryEnrollmentReview {
    pub fn grants(&self) -> &[NativeEnrollmentMetadataGrant] {
        &self.grants
    }

    pub fn control_sinks(&self) -> &BTreeSet<TargetRef> {
        &self.sinks
    }
}

/// Frozen input to a separate enrollment-scoped native metadata decision.
#[derive(Debug)]
pub struct EnrollmentMetadataGrantProposal {
    id: ApprovalId,
    connection: ConnectionId,
    registration: ControllerRegistrationId,
    enrollment: ControllerEnrollmentId,
    target: TargetRef,
    controls: BTreeSet<TargetRef>,
    control_grant: Option<NativeControlGrant>,
}

impl EnrollmentMetadataGrantProposal {
    pub fn approval_id(&self) -> ApprovalId {
        self.id
    }

    pub fn registration(&self) -> ControllerRegistrationId {
        self.registration
    }

    pub fn enrollment(&self) -> ControllerEnrollmentId {
        self.enrollment
    }

    pub fn target(&self) -> TargetRef {
        self.target
    }

    pub fn control_sinks(&self) -> impl Iterator<Item = TargetRef> + '_ {
        self.controls.iter().copied()
    }

    pub fn control_grant(&self) -> Option<NativeControlGrant> {
        self.control_grant
    }
}

/// Trusted native decision for one exact enrollment, never an IPC credential.
///
/// ```compile_fail
/// use festerm_automation::NativeEnrollmentMetadataGrantApproval;
/// let _ = serde_json::from_str::<NativeEnrollmentMetadataGrantApproval>("{}");
/// ```
pub struct NativeEnrollmentMetadataGrantApproval {
    proposal: EnrollmentMetadataGrantProposal,
    lifetime: Duration,
    control_sinks: BTreeSet<TargetRef>,
    control_disclosures: BTreeMap<NativeControlGrant, BTreeSet<TargetRef>>,
}

impl NativeEnrollmentMetadataGrantApproval {
    pub fn new(proposal: EnrollmentMetadataGrantProposal, lifetime: Duration) -> Self {
        Self {
            proposal,
            lifetime,
            control_sinks: BTreeSet::new(),
            control_disclosures: BTreeMap::new(),
        }
    }

    /// Explicitly acknowledge this exact metadata source at a control sink.
    pub fn accept_control_sink(mut self, sink: TargetRef) -> Self {
        self.control_sinks.insert(sink);
        self
    }

    /// Control-side acknowledgement for the reviewed existing grant and sink.
    pub fn accept_existing_control_sink(
        mut self,
        control: NativeControlGrant,
        sink: TargetRef,
    ) -> Self {
        self.control_disclosures
            .entry(control)
            .or_default()
            .insert(sink);
        self
    }
}

/// A bounded, host-only review candidate. It is not disclosure authority.
#[derive(Debug)]
pub struct EnrollmentMetadataReview {
    connection: ConnectionId,
    registration: ControllerRegistrationId,
    enrollment: ControllerEnrollmentId,
    target: TargetRef,
    review: u64,
    ordinary_revision: u64,
    ownership: BTreeMap<TargetRef, (u64, Option<LeaseRef>)>,
    expires: Instant,
}

impl EnrollmentMetadataReview {
    pub fn registration(&self) -> ControllerRegistrationId {
        self.registration
    }

    pub fn enrollment(&self) -> ControllerEnrollmentId {
        self.enrollment
    }

    pub fn target(&self) -> TargetRef {
        self.target
    }

    pub fn control_sinks(&self) -> impl Iterator<Item = TargetRef> + '_ {
        self.ownership.keys().copied()
    }
}

/// Native-only model input; it is not a native UI or an IPC credential.
///
/// ```compile_fail
/// use festerm_automation::NativeEnrollmentMetadataApproval;
/// let _ = serde_json::from_str::<NativeEnrollmentMetadataApproval>("{}");
/// ```
pub struct NativeEnrollmentMetadataApproval {
    review: EnrollmentMetadataReview,
    lifetime: Duration,
    control_sinks: BTreeSet<TargetRef>,
}

impl NativeEnrollmentMetadataApproval {
    pub fn new(review: EnrollmentMetadataReview, lifetime: Duration) -> Self {
        Self {
            review,
            lifetime,
            control_sinks: BTreeSet::new(),
        }
    }

    /// Explicitly acknowledge the reviewed metadata source flowing to a sink.
    pub fn accept_control_sink(mut self, sink: TargetRef) -> Self {
        self.control_sinks.insert(sink);
        self
    }
}

/// Identity of a modeled decision, never a permit to disclose or start a run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnrollmentMetadataDecision {
    instance: [u8; 16],
    sequence: u64,
    enrollment: ControllerEnrollmentId,
    ordinary_revision: u64,
    expires: Instant,
}

impl Broker {
    /// Request a bounded review using the same pending and lifetime quotas as
    /// ordinary and legacy metadata proposals.
    pub fn request_enrollment_metadata_approval(
        &mut self,
        connection: ConnectionId,
        registration: ControllerRegistrationId,
        enrollment: ControllerEnrollmentId,
        now: Instant,
        _guard: &AutomationAdmissionGuard,
    ) -> Result<EnrollmentMetadataGrantProposal> {
        self.poll_expiry(now);
        let target = self.resolve_controller_enrollment(connection, registration, enrollment)?;
        let principal = self
            .principals
            .get(&connection)
            .ok_or(Error::Unauthorized)?;
        let controls = principal
            .grant
            .as_ref()
            .map_or_else(BTreeSet::new, |grant| grant.request.scope.control.clone());
        let control_grant = principal.grant.as_ref().and_then(|grant| {
            (!controls.is_empty()).then_some(NativeControlGrant {
                instance: self.instance,
                epoch: grant.epoch,
            })
        });
        let ownership = self.ownership_snapshot(&controls)?;
        let id = self.request_pending(
            connection,
            PendingProposal::EnrollmentMetadata {
                registration,
                enrollment,
                target,
            },
            ownership,
            now,
        )?;
        Ok(EnrollmentMetadataGrantProposal {
            id,
            connection,
            registration,
            enrollment,
            target,
            controls,
            control_grant,
        })
    }

    /// Native review of an ordinary proposal must display these exact active
    /// enrollments before acknowledging their metadata at control sinks.
    pub fn prepare_ordinary_enrollment_review(
        &self,
        connection: ConnectionId,
        id: ApprovalId,
        sinks: BTreeSet<TargetRef>,
        now: Instant,
        _guard: &AutomationAdmissionGuard,
    ) -> Result<NativeOrdinaryEnrollmentReview> {
        let pending = self.pending.get(&id).ok_or(Error::Unauthorized)?;
        let PendingProposal::Ordinary(proposal) = &pending.proposal else {
            return Err(Error::Unauthorized);
        };
        if pending.connection != connection || !sinks.is_subset(&proposal.scope.control) {
            return Err(Error::Unauthorized);
        }
        if now >= pending.expires || pending.revisions != self.revisions(connection)? {
            return Err(Error::Revoked);
        }
        let principal = self
            .principals
            .get(&connection)
            .ok_or(Error::Unauthorized)?;
        if principal.controller_registrations.values().any(|record| {
            record
                .metadata_grant
                .as_ref()
                .is_some_and(|grant| now >= grant.expires)
        }) {
            return Err(Error::Expired);
        }
        Ok(NativeOrdinaryEnrollmentReview {
            connection,
            id,
            revisions: pending.revisions,
            ordinary_epoch: principal.grant.as_ref().map(|grant| grant.epoch),
            grants: principal.enrollment_metadata_sources(connection),
            sinks,
        })
    }

    /// Install one discovery-only grant for the exact reviewed enrollment.
    pub fn approve_enrollment_metadata_grant(
        &mut self,
        connection: ConnectionId,
        approval: NativeEnrollmentMetadataGrantApproval,
        now: Instant,
        _guard: &AutomationAdmissionGuard,
    ) -> Result<()> {
        self.poll_expiry(now);
        let proposal = approval.proposal;
        if proposal.connection != connection {
            return Err(Error::Unauthorized);
        }
        let pending = self.pending.get(&proposal.id).ok_or(Error::Unauthorized)?;
        if pending.connection != connection
            || !matches!(
                pending.proposal,
                PendingProposal::EnrollmentMetadata { registration, enrollment, target }
                    if registration == proposal.registration
                        && enrollment == proposal.enrollment
                        && target == proposal.target
            )
        {
            return Err(Error::Unauthorized);
        }
        if pending.revisions != self.revisions(connection)? {
            return Err(Error::Revoked);
        }
        if approval.lifetime.is_zero() || approval.lifetime > MAX_GRANT_LIFETIME {
            return Err(Error::InvalidRequest);
        }
        if self.resolve_controller_enrollment(
            connection,
            proposal.registration,
            proposal.enrollment,
        )? != proposal.target
        {
            return Err(Error::Revoked);
        }
        let principal = self
            .principals
            .get(&connection)
            .ok_or(Error::Unauthorized)?;
        let controls = principal
            .grant
            .as_ref()
            .map_or_else(BTreeSet::new, |grant| grant.request.scope.control.clone());
        if controls != proposal.controls
            || controls != approval.control_sinks
            || pending.ownership != self.ownership_snapshot(&controls)?
        {
            return Err(Error::Unauthorized);
        }
        let current_control = principal.grant.as_ref().and_then(|grant| {
            (!controls.is_empty()).then_some(NativeControlGrant {
                instance: self.instance,
                epoch: grant.epoch,
            })
        });
        if current_control != proposal.control_grant {
            return Err(Error::Revoked);
        }
        match current_control {
            Some(control)
                if approval.control_disclosures.len() == 1
                    && approval.control_disclosures.get(&control) == Some(&controls) => {}
            None if approval.control_disclosures.is_empty() => {}
            _ => return Err(Error::Unauthorized),
        }
        let targets = BTreeSet::from([proposal.target]);
        if let Some(grant) = &principal.grant {
            Self::validate_cross_closure(&grant.request.scope, &grant.handoffs, &targets)?;
        }
        let slot = GrantSlot::EnrollmentMetadata(proposal.registration, proposal.enrollment);
        self.validate_revision_transition(connection, slot)?;
        let expires = now
            .checked_add(approval.lifetime)
            .ok_or(Error::InvalidRequest)?;
        let epoch = self.mint()?;
        self.revoke_slot(connection, slot);
        let principal = self
            .principals
            .get_mut(&connection)
            .ok_or(Error::Unauthorized)?;
        if !principal.has_any_grant() {
            principal.usage = Usage::default();
        }
        principal.revisions.enrollment_metadata += 1;
        let record = principal
            .controller_registrations
            .get_mut(&proposal.registration)
            .ok_or(Error::Unauthorized)?;
        record.metadata_grant = Some(MetadataGrant {
            epoch,
            targets,
            expires,
            usage: Usage::default(),
            delivery: DeliveryEpoch::new(),
        });
        record.metadata_cross_consent = current_control.map(|control| EnrollmentCrossConsent {
            ordinary_epoch: control.epoch,
            metadata_epoch: epoch,
            sinks: controls,
        });
        self.discard_stale_proposals(connection);
        Ok(())
    }

    /// Admit only exact-enrollment discovery; legacy metadata is never used.
    pub fn admit_enrollment_metadata_run(
        &mut self,
        connection: ConnectionId,
        registration: ControllerRegistrationId,
        enrollment: ControllerEnrollmentId,
        spec: MetadataRunSpec,
        now: Instant,
    ) -> Result<RunId> {
        let target = self.resolve_controller_enrollment(connection, registration, enrollment)?;
        if spec.targets != BTreeSet::from([target]) {
            return Err(Error::Unauthorized);
        }
        let spec = RunSpec {
            binding: InvocationBinding {
                api_version: 1,
                invocation: Invocation::Direct {
                    operation: DirectKind::Discovery,
                },
                parameters: spec.parameters,
            },
            scope: Scope {
                discover: spec.targets,
                ..Scope::default()
            },
            budget: spec.budget,
            deadline: spec.deadline,
        };
        self.admit_in_slot(
            connection,
            GrantSlot::EnrollmentMetadata(registration, enrollment),
            None,
            spec,
            now,
        )
    }

    /// Identify the current exact-enrollment grant without authorizing a read.
    pub fn native_enrollment_metadata_grant(
        &self,
        connection: ConnectionId,
        registration: ControllerRegistrationId,
        enrollment: ControllerEnrollmentId,
        now: Instant,
    ) -> Result<NativeEnrollmentMetadataGrant> {
        let target = self.resolve_controller_enrollment(connection, registration, enrollment)?;
        let GrantRef::Metadata(grant) = self.slot_grant(
            connection,
            GrantSlot::EnrollmentMetadata(registration, enrollment),
            now,
        )?
        else {
            return Err(Error::Revoked);
        };
        Ok(NativeEnrollmentMetadataGrant {
            connection,
            registration,
            enrollment,
            target,
            epoch: grant.epoch,
            expires: grant.expires,
        })
    }

    /// Revoke this exact grant, pending decisions, runs and liveness only.
    pub fn revoke_enrollment_metadata_grant(
        &mut self,
        connection: ConnectionId,
        registration: ControllerRegistrationId,
        enrollment: ControllerEnrollmentId,
        _guard: &AutomationAdmissionGuard,
    ) -> Result<()> {
        self.resolve_controller_enrollment(connection, registration, enrollment)?;
        self.revoke_slot(
            connection,
            GrantSlot::EnrollmentMetadata(registration, enrollment),
        );
        Ok(())
    }

    /// Freeze one exact enrollment and the current ordinary control closure.
    /// Only one review per registration may be outstanding.
    pub fn prepare_enrollment_metadata_review(
        &mut self,
        connection: ConnectionId,
        registration: ControllerRegistrationId,
        enrollment: ControllerEnrollmentId,
        now: Instant,
        _guard: &AutomationAdmissionGuard,
    ) -> Result<EnrollmentMetadataReview> {
        self.poll_expiry(now);
        let target = self.resolve_controller_enrollment(connection, registration, enrollment)?;
        let principal = self
            .principals
            .get(&connection)
            .ok_or(Error::Unauthorized)?;
        let ordinary_revision = principal.revisions.ordinary;
        let controls = principal
            .grant
            .as_ref()
            .map_or_else(BTreeSet::new, |grant| grant.request.scope.control.clone());
        let ownership = self.ownership_snapshot(&controls)?;
        let expires = now
            .checked_add(MAX_GRANT_LIFETIME)
            .ok_or(Error::InvalidRequest)?;
        let review = self.mint()?;
        self.principals
            .get_mut(&connection)
            .and_then(|p| p.controller_registrations.get_mut(&registration))
            .ok_or(Error::Unauthorized)?
            .metadata_review = Some(review);
        Ok(EnrollmentMetadataReview {
            connection,
            registration,
            enrollment,
            target,
            review,
            ordinary_revision,
            ownership,
            expires,
        })
    }

    /// Cancel only this exact candidate, without affecting another enrollment.
    pub fn cancel_enrollment_metadata_review(
        &mut self,
        connection: ConnectionId,
        review: EnrollmentMetadataReview,
        _guard: &AutomationAdmissionGuard,
    ) -> Result<()> {
        if review.connection != connection {
            return Err(Error::Unauthorized);
        }
        let record = self
            .principals
            .get_mut(&connection)
            .and_then(|p| p.controller_registrations.get_mut(&review.registration))
            .ok_or(Error::Unauthorized)?;
        if record.metadata_review != Some(review.review) {
            return Err(Error::Revoked);
        }
        record.metadata_review = None;
        Ok(())
    }

    /// Record a modeled decision. This does not install a broker grant.
    pub fn approve_enrollment_metadata_review(
        &mut self,
        connection: ConnectionId,
        approval: NativeEnrollmentMetadataApproval,
        now: Instant,
        _guard: &AutomationAdmissionGuard,
    ) -> Result<EnrollmentMetadataDecision> {
        self.poll_expiry(now);
        let review = approval.review;
        if review.connection != connection {
            return Err(Error::Unauthorized);
        }
        if now >= review.expires {
            return Err(Error::Expired);
        }
        if approval.lifetime.is_zero() || approval.lifetime > MAX_GRANT_LIFETIME {
            return Err(Error::InvalidRequest);
        }
        if self.resolve_controller_enrollment(connection, review.registration, review.enrollment)?
            != review.target
        {
            return Err(Error::Revoked);
        }
        let principal = self
            .principals
            .get(&connection)
            .ok_or(Error::Unauthorized)?;
        let record = principal
            .controller_registrations
            .get(&review.registration)
            .ok_or(Error::Unauthorized)?;
        if record.metadata_review != Some(review.review)
            || principal.revisions.ordinary != review.ordinary_revision
        {
            return Err(Error::Revoked);
        }
        let controls: BTreeSet<_> = review.ownership.keys().copied().collect();
        if approval.control_sinks != controls
            || self.ownership_snapshot(&controls)? != review.ownership
        {
            return Err(Error::Unauthorized);
        }
        if let Some(grant) = &principal.grant {
            Self::validate_cross_closure(
                &grant.request.scope,
                &grant.handoffs,
                &BTreeSet::from([review.target]),
            )?;
        }
        let expires = now
            .checked_add(approval.lifetime)
            .ok_or(Error::InvalidRequest)?;
        let decision = EnrollmentMetadataDecision {
            instance: self.instance,
            sequence: self.mint()?,
            enrollment: review.enrollment,
            ordinary_revision: review.ordinary_revision,
            expires,
        };
        let record = self
            .principals
            .get_mut(&connection)
            .and_then(|p| p.controller_registrations.get_mut(&review.registration))
            .ok_or(Error::Unauthorized)?;
        record.metadata_review = None;
        record.metadata_decision = Some(decision);
        Ok(decision)
    }

    /// Inspect model liveness only. Never use this as a publication check.
    pub fn check_enrollment_metadata_decision(
        &self,
        connection: ConnectionId,
        registration: ControllerRegistrationId,
        enrollment: ControllerEnrollmentId,
        decision: EnrollmentMetadataDecision,
        now: Instant,
    ) -> Result<()> {
        self.resolve_controller_enrollment(connection, registration, enrollment)?;
        let principal = self
            .principals
            .get(&connection)
            .ok_or(Error::Unauthorized)?;
        let record = principal
            .controller_registrations
            .get(&registration)
            .ok_or(Error::Unauthorized)?;
        if record.metadata_decision != Some(decision)
            || decision.enrollment != enrollment
            || principal.revisions.ordinary != decision.ordinary_revision
        {
            return Err(Error::Revoked);
        }
        if now >= decision.expires {
            return Err(Error::Expired);
        }
        if principal
            .grant
            .as_ref()
            .is_some_and(|grant| now >= grant.expires)
        {
            return Err(Error::Revoked);
        }
        Ok(())
    }

    /// Revoke this registration's modeled decision and review only.
    pub fn revoke_enrollment_metadata_decision(
        &mut self,
        connection: ConnectionId,
        registration: ControllerRegistrationId,
        _guard: &AutomationAdmissionGuard,
    ) -> Result<()> {
        let record = self
            .principals
            .get_mut(&connection)
            .and_then(|p| p.controller_registrations.get_mut(&registration))
            .ok_or(Error::Unauthorized)?;
        record.metadata_review = None;
        record.metadata_decision = None;
        Ok(())
    }
}
