//! Real local session-control adapter, still unexposed remotely.
//!
//! Standalone composition advertises only inventory. The authenticated runtime
//! also supplies adopted terminal routes before advertising lifecycle control.
use super::{
    InventoryCapture, OperationAdmission, PolicyGuard, ProtectedPolicySource, SessionBackend,
};
use festerm_host_protocol::{
    AuthorizedInventory, Capabilities, ContractError, EnsureDisposition, Envelope, Epoch,
    InventoryEntry, LogicalState, LookupResult, Refusal, Request, Response, SessionGrant,
    UnknownInventoryEntry, CONTROL_DEADLINE_SECONDS,
};
use festerm_sessiond::host_control::{
    AdmissionState, ControlError, Effect, EndOutcome, Lookup, SessionControl,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub struct NativeControl<A: ProtectedPolicySource> {
    sessions: Arc<SessionControl>,
    policy: Arc<A>,
    terminal_routes: bool,
    #[cfg(all(test, feature = "relay", unix))]
    test_capabilities: bool,
}

impl<A: ProtectedPolicySource> NativeControl<A> {
    pub fn new(sessions: impl Into<Arc<SessionControl>>, policy: Arc<A>) -> Self {
        Self {
            sessions: sessions.into(),
            policy,
            terminal_routes: false,
            #[cfg(all(test, feature = "relay", unix))]
            test_capabilities: false,
        }
    }

    pub(crate) fn with_terminal_routes(sessions: Arc<SessionControl>, policy: Arc<A>) -> Self {
        Self {
            sessions,
            policy,
            terminal_routes: true,
            #[cfg(all(test, feature = "relay", unix))]
            test_capabilities: false,
        }
    }

    #[cfg(all(test, feature = "relay", unix))]
    pub(super) fn with_test_capabilities(sessions: SessionControl, policy: Arc<A>) -> Self {
        Self {
            sessions: sessions.into(),
            policy,
            terminal_routes: false,
            test_capabilities: true,
        }
    }

    fn advertised(&self, current: Capabilities) -> Capabilities {
        if self.terminal_routes {
            return current;
        }
        #[cfg(all(test, feature = "relay", unix))]
        if self.test_capabilities {
            return current;
        }
        Capabilities {
            inventory: current.inventory,
            ..Capabilities::default()
        }
    }

    fn supports(&self, request: &Request) -> bool {
        if self.terminal_routes {
            // Attach and Detach use their independently authenticated terminal
            // stream, never acquire a frontend on this control connection.
            return !matches!(request, Request::Attach { .. } | Request::Detach { .. });
        }
        #[cfg(all(test, feature = "relay", unix))]
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
        self.authorize_state(admission, state, request)
            .map(|(guard, _)| guard)
    }

    fn authorize_state(
        &self,
        admission: &OperationAdmission,
        state: &AdmissionState,
        request: &Envelope<Request>,
    ) -> Result<(A::Guard, Capabilities), ControlError> {
        let guard = self.policy.current_policy().map_err(control_error)?;
        let projected = state
            .issuance_epoch
            .map(|epoch| guard.verifier().with_issuance_epoch(epoch))
            .transpose()
            .map_err(control_error)?;
        let verifier = projected.as_ref().unwrap_or(guard.verifier());
        if matches!(
            request.body,
            Request::Ensure { .. } | Request::Create { .. }
        ) {
            let issued = request.issued_operation().map_err(control_error)?;
            if state
                .issuance_epoch
                .is_none_or(|current| issued.epoch.get() > current.get())
            {
                return Err(ControlError::Stale);
            }
        }
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
                effective_request.issuance_epoch = None;
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
                verifier,
                &effective_request,
                state.grant.as_ref().or(provisional.as_ref()),
                state
                    .grant_epoch
                    .unwrap_or(Epoch::new(1).map_err(control_error)?),
            )
            .map_err(control_error)?;
        let capabilities = verifier.capabilities();
        Ok((guard, capabilities))
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
        let mut grants: Vec<_> = current
            .sessions
            .iter()
            .map(|session| session.grant.clone())
            .chain(current.unknown.iter().map(|session| session.grant.clone()))
            .collect();
        grants.sort_by(|a, b| a.logical.cmp(&b.logical));
        let unknown: Vec<_> = current
            .unknown
            .iter()
            .map(|session| session.target.clone())
            .collect();
        let mut retained = admission.inventory.lock().map_err(|_| Refusal::Denied)?;
        if admission.cancelled() {
            return Err(Refusal::Stale);
        }
        if cursor.is_none() {
            let entries = current.sessions.into_iter().map(|session| InventoryEntry {
                owner: session.grant.owner,
                shared: session.grant.remote_access,
                revoked: matches!(session.grant.state, LogicalState::Revoked),
                session: session.summary,
            });
            let uncertain = current
                .unknown
                .into_iter()
                .map(|session| UnknownInventoryEntry {
                    owner: session.grant.owner,
                    shared: session.grant.remote_access,
                    revoked: matches!(session.grant.state, LogicalState::Revoked),
                    target: session.target,
                });
            let snapshot = AuthorizedInventory::capture_with_unknowns(
                request.host.clone(),
                admission.principal().clone(),
                fence.grant_epoch,
                request.operation.clone(),
                entries,
                uncertain,
            )
            .map_err(|error| refusal(control_error(error)))?;
            *retained = Some(InventoryCapture {
                snapshot,
                grants: grants.clone(),
                unknown: unknown.clone(),
                expires: Instant::now() + Duration::from_secs(u64::from(CONTROL_DEADLINE_SECONDS)),
            });
        }
        let capture = retained.as_ref().ok_or(Refusal::Stale)?;
        if Instant::now() >= capture.expires
            || !same_grants(&capture.grants, &grants)
            || capture.unknown != unknown
        {
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
        // not an advertisement. Standalone composition remains inventory-only.
        if !self.supports(&request.body) {
            return Err(Refusal::Unsupported);
        }
        match &request.body {
            Request::Capabilities {} => {
                let mut captured = None;
                let epoch = self
                    .sessions
                    .issuance_epoch(admission.principal(), |state| {
                        let (guard, capabilities) =
                            self.authorize_state(admission, state, request)?;
                        captured = Some((
                            state.issuance_epoch.ok_or(ControlError::Unknown)?,
                            capabilities,
                        ));
                        Ok(guard)
                    })
                    .map_err(refusal)?;
                let (captured_epoch, capabilities) = captured.ok_or(Refusal::Unknown)?;
                if captured_epoch != epoch {
                    return Err(Refusal::Busy);
                }
                Ok(Response::Capabilities {
                    capabilities: self.advertised(capabilities),
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
                        &request
                            .issued_operation()
                            .map_err(control_error)
                            .map_err(refusal)?,
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
                let outcome = self
                    .sessions
                    .end(admission.principal(), target, |state| {
                        self.admit(admission, state, request)
                    })
                    .map_err(refusal)?;
                match outcome {
                    EndOutcome::StopRequested => Err(Refusal::OutcomeUncertain),
                    EndOutcome::ObservedEnded => Ok(Response::Ended {
                        target: target.clone(),
                    }),
                }
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
