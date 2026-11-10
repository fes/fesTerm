//! Real local session-control adapter, still unexposed remotely.
//!
//! Only inventory is advertised until the adopted terminal relay is integrated.
//! Attachment and detach refuse rather than manufacture a frontend lease.
use super::{
    InventoryCapture, OperationAdmission, PolicyGuard, ProtectedPolicySource, SessionBackend,
};
use festerm_host_protocol::{
    AuthorizedInventory, Capabilities, ContractError, EnsureDisposition, Envelope, Epoch,
    InventoryEntry, LogicalState, LookupResult, Refusal, Request, Response, SessionGrant,
    CONTROL_DEADLINE_SECONDS,
};
use festerm_sessiond::host_control::{
    AdmissionState, ControlError, Effect, Lookup, SessionControl,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub struct NativeControl<A: ProtectedPolicySource> {
    sessions: SessionControl,
    policy: Arc<A>,
    #[cfg(test)]
    test_capabilities: bool,
}

impl<A: ProtectedPolicySource> NativeControl<A> {
    pub fn new(sessions: SessionControl, policy: Arc<A>) -> Self {
        Self {
            sessions,
            policy,
            #[cfg(test)]
            test_capabilities: false,
        }
    }

    #[cfg(test)]
    pub(super) fn with_test_capabilities(sessions: SessionControl, policy: Arc<A>) -> Self {
        Self {
            sessions,
            policy,
            test_capabilities: true,
        }
    }

    fn advertised(&self, current: Capabilities) -> Capabilities {
        #[cfg(test)]
        if self.test_capabilities {
            return current;
        }
        Capabilities {
            inventory: current.inventory,
            ..Capabilities::default()
        }
    }

    fn supports(&self, request: &Request) -> bool {
        #[cfg(test)]
        if self.test_capabilities {
            return true;
        }
        Capabilities {
            inventory: true,
            ..Capabilities::default()
        }
        .supports(request)
    }

    fn admit(
        &self,
        admission: &OperationAdmission,
        state: &AdmissionState,
        request: &Envelope<Request>,
    ) -> Result<A::Guard, ControlError> {
        let guard = self.policy.current_policy().map_err(control_error)?;
        let mut effective_request = request.clone();
        if matches!(request.body, Request::Create { .. }) && state.effect == Effect::Resume {
            if let Some(SessionGrant {
                state: LogicalState::Running(generation),
                logical,
                ..
            }) = &state.grant
            {
                if !guard.verifier().policy().capabilities.create
                    || !guard.verifier().policy().capabilities.creation_allowed
                {
                    return Err(ControlError::Denied);
                }
                let Request::Create { creation } = &request.body else {
                    unreachable!()
                };
                // A journaled Create retry/publication is an exact committed
                // generation, not a second absent-ID creation admission.
                effective_request.body = Request::Attach {
                    target: festerm_host_protocol::Target {
                        logical: logical.clone(),
                        generation: *generation,
                    },
                    attachment: creation.attachment.clone(),
                    takeover: false,
                };
            }
        }
        let mut provisional = None;
        if state.grant.is_none() && matches!(state.effect, Effect::Create | Effect::Resume) {
            if let Request::Ensure { creation } | Request::Create { creation } = &request.body {
                // This is the backend's locked virgin-ID reservation callback,
                // not an absence reply or a grant reconstructed from config.
                // Ensure may resume without creation permission, but a virgin
                // identity always needs the current independent execution gate.
                if !guard.verifier().policy().capabilities.creation_allowed {
                    return Err(ControlError::Denied);
                }
                provisional = Some(SessionGrant {
                    logical: creation.logical.clone(),
                    owner: admission.principal().clone(),
                    remote_access: true,
                    state: LogicalState::Absent,
                    revision: Epoch::new(1).map_err(control_error)?,
                });
            }
        }
        admission
            .authorize_current(
                guard.verifier(),
                &effective_request,
                state.grant.as_ref().or(provisional.as_ref()),
                Epoch::new(1).map_err(control_error)?,
            )
            .map_err(control_error)?;
        Ok(guard)
    }

    fn inventory(
        &self,
        request: &Envelope<Request>,
        admission: &OperationAdmission,
        cursor: Option<&festerm_host_protocol::Cursor>,
    ) -> Result<Response, Refusal> {
        let current = self
            .sessions
            .inventory(admission.principal(), |state| {
                self.admit(admission, state, request)
            })
            .map_err(refusal)?;
        let guard = self
            .policy
            .current_policy()
            .map_err(|error| refusal(control_error(error)))?;
        let fence = admission
            .authorize_current(
                guard.verifier(),
                request,
                None,
                Epoch::new(1).map_err(|_| Refusal::Unknown)?,
            )
            .map_err(|error| refusal(control_error(error)))?;
        let grants: Vec<_> = current
            .iter()
            .map(|session| session.grant.clone())
            .collect();
        let mut retained = admission.inventory.lock().map_err(|_| Refusal::Denied)?;
        if admission.cancelled() {
            return Err(Refusal::Stale);
        }
        if cursor.is_none() {
            let entries = current.into_iter().map(|session| InventoryEntry {
                owner: session.grant.owner,
                shared: session.grant.remote_access,
                revoked: matches!(session.grant.state, LogicalState::Revoked),
                session: session.summary,
            });
            let snapshot = AuthorizedInventory::capture(
                request.host.clone(),
                admission.principal().clone(),
                fence.grant_epoch,
                request.operation.clone(),
                entries,
            )
            .map_err(|error| refusal(control_error(error)))?;
            *retained = Some(InventoryCapture {
                snapshot,
                grants: grants.clone(),
                expires: Instant::now() + Duration::from_secs(u64::from(CONTROL_DEADLINE_SECONDS)),
            });
        }
        let capture = retained.as_ref().ok_or(Refusal::Stale)?;
        if Instant::now() >= capture.expires || !same_grants(&capture.grants, &grants) {
            *retained = None;
            return Err(Refusal::Stale);
        }
        let page = capture
            .snapshot
            .page(
                &request.host,
                admission.principal(),
                fence.grant_epoch,
                cursor,
            )
            .map_err(|error| refusal(control_error(error)))?;
        Ok(Response::Inventory { page })
    }
}

impl<A: ProtectedPolicySource> SessionBackend for NativeControl<A> {
    fn execute(
        &self,
        request: &Envelope<Request>,
        admission: &OperationAdmission,
    ) -> Result<Response, Refusal> {
        // Defense in depth for direct backend composition: executable code is
        // not an advertisement. Production remains inventory-only.
        if !self.supports(&request.body) {
            return Err(Refusal::Unsupported);
        }
        match &request.body {
            Request::Capabilities {} => {
                let guard = self
                    .policy
                    .current_policy()
                    .map_err(|error| refusal(control_error(error)))?;
                admission
                    .authorize_current(
                        guard.verifier(),
                        request,
                        None,
                        Epoch::new(1).map_err(|_| Refusal::Unknown)?,
                    )
                    .map_err(|error| refusal(control_error(error)))?;
                Ok(Response::Capabilities {
                    capabilities: self.advertised(guard.verifier().policy().capabilities),
                })
            }
            Request::List { cursor } => self.inventory(request, admission, cursor.as_ref()),
            Request::Lookup { logical } => {
                let mut authorization_epoch = None;
                let result = self
                    .sessions
                    .inspect(admission.principal(), logical, |state| {
                        let guard = self.admit(admission, state, request)?;
                        authorization_epoch = Some(guard.verifier().policy().grant_epoch);
                        Ok(guard)
                    })
                    .map_err(refusal)?;
                let result = match result.state {
                    Lookup::Running(_) => LookupResult::Running {
                        session: result.summary.ok_or(Refusal::Unknown)?,
                    },
                    Lookup::Absent => LookupResult::Absent {
                        logical: logical.clone(),
                        authorization_epoch: authorization_epoch.ok_or(Refusal::Unknown)?,
                    },
                    Lookup::Unknown => return Err(Refusal::Unknown),
                    Lookup::Revoked => return Err(Refusal::Denied),
                };
                Ok(Response::Lookup { result })
            }
            Request::Ensure { creation } | Request::Create { creation } => {
                let result = self
                    .sessions
                    .ensure(
                        admission.principal(),
                        &request.operation,
                        creation,
                        matches!(request.body, Request::Create { .. }),
                        &admission.cancelled,
                        |state| self.admit(admission, state, request),
                    )
                    .map_err(refusal)?;
                if result.cancelled {
                    return Err(Refusal::OutcomeUncertain);
                }
                Ok(Response::Ensured {
                    session: result.summary,
                    disposition: if result.recreated {
                        EnsureDisposition::Recreated
                    } else if result.created {
                        EnsureDisposition::Created
                    } else {
                        EnsureDisposition::Resumed
                    },
                    lease: result.lease,
                })
            }
            Request::ExplicitEnd { target } => {
                self.sessions
                    .end(admission.principal(), target, |state| {
                        self.admit(admission, state, request)
                    })
                    .map_err(refusal)?;
                // StopRequested is deliberately not an Ended success.
                Err(Refusal::OutcomeUncertain)
            }
            // A control connection never silently acquires a frontend lease.
            Request::Attach { .. } | Request::Detach { .. } => Err(Refusal::Unsupported),
        }
    }
}

fn same_grants(a: &[SessionGrant], b: &[SessionGrant]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.logical == b.logical
                && a.owner == b.owner
                && a.remote_access == b.remote_access
                && a.state == b.state
                && a.revision == b.revision
        })
}

fn control_error(error: ContractError) -> ControlError {
    match error {
        ContractError::Denied => ControlError::Denied,
        ContractError::Busy => ControlError::Busy,
        ContractError::Stale => ControlError::Stale,
        _ => ControlError::Invalid,
    }
}

fn refusal(error: ControlError) -> Refusal {
    match error {
        ControlError::Denied | ControlError::Revoked => Refusal::Denied,
        ControlError::Busy | ControlError::AlreadyExists => Refusal::Busy,
        ControlError::Stale | ControlError::Ended | ControlError::Cancelled => Refusal::Stale,
        ControlError::RecipeConflict | ControlError::OperationConflict => Refusal::RecipeConflict,
        ControlError::CommittedButNotPublished => Refusal::OutcomeUncertain,
        ControlError::Unknown | ControlError::Storage | ControlError::Invalid => Refusal::Unknown,
    }
}
