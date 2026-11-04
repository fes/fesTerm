use super::*;
use std::time::Duration;

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
