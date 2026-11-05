use super::*;
use festerm_automation::NativeControlGrant;
use festerm_session::SessionLifecycle;

const MAX_ISSUED_IDS: usize = 256;
const MAX_ID_ATTEMPTS: usize = 8;
const MAX_LOCAL_TARGETS: usize = 128;
const MAX_CONTROLLER_REQUESTS: usize = 256;
const MAX_ENROLLMENT_METADATA_BYTES: u32 = 2_048;

struct Registration {
    internal: ControllerRegistrationId,
    valid: Arc<AtomicBool>,
    association: Option<Association>,
    direct_control: Option<(TargetRef, NativeControlGrant)>,
}

#[derive(Clone, Copy)]
struct Association {
    public: ControllerId,
    enrollment: ControllerEnrollmentId,
    target: TargetRef,
    window: WindowId,
}

#[derive(Clone)]
pub(super) struct FrozenAssociation {
    principal: ConnectionId,
    registration: ControllerRegistrationId,
    enrollment: ControllerEnrollmentId,
    public: ControllerId,
    target: TargetRef,
    window: WindowId,
    valid: Arc<AtomicBool>,
}

impl FrozenAssociation {
    pub(super) fn target(&self) -> TargetRef {
        self.target
    }

    pub(super) fn bind_direct_control_grant(
        &self,
        peer: &mut Peer,
        grant: NativeControlGrant,
    ) -> festerm_automation::Result<()> {
        let record = peer
            .controller
            .registrations
            .values_mut()
            .find(|record| record.internal == self.registration)
            .ok_or(festerm_automation::Error::Revoked)?;
        if peer.principal != self.principal
            || !self.valid.load(Ordering::Acquire)
            || !Arc::ptr_eq(&record.valid, &self.valid)
            || !record.association.is_some_and(|current| {
                current.public == self.public
                    && current.enrollment == self.enrollment
                    && current.target == self.target
                    && current.window == self.window
            })
        {
            return Err(festerm_automation::Error::Revoked);
        }
        record.direct_control = Some((self.target, grant));
        Ok(())
    }

    pub(super) fn matches_handle(&self, handle: ControllerId) -> bool {
        self.public == handle
    }

    pub(super) fn valid_flag(&self) -> Arc<AtomicBool> {
        self.valid.clone()
    }

    pub(super) fn is_bound(
        &self,
        broker: &Broker,
        targets: &BTreeMap<u64, Target>,
        peer: &Peer,
        _guard: &festerm_session::AutomationAdmissionGuard,
    ) -> bool {
        peer.principal == self.principal
            && self.valid.load(Ordering::Acquire)
            && peer.controller.registrations.values().any(|record| {
                record.internal == self.registration
                    && Arc::ptr_eq(&record.valid, &self.valid)
                    && record.association.is_some_and(|association| {
                        association.public == self.public
                            && association.enrollment == self.enrollment
                            && association.target == self.target
                            && association.window == self.window
                    })
            })
            && broker.resolve_controller_enrollment(
                self.principal,
                self.registration,
                self.enrollment,
            ) == Ok(self.target)
            && targets.get(&self.target.session()).is_some_and(|target| {
                target.reference == self.target && target.local_pty && target.guard().is_valid()
            })
    }

    pub(super) fn is_current(
        &self,
        broker: &Broker,
        targets: &BTreeMap<u64, Target>,
        peer: &Peer,
        apps: &[&FesTermApp],
        guard: &festerm_session::AutomationAdmissionGuard,
    ) -> bool {
        self.is_bound(broker, targets, peer, guard)
            && exact_target(targets, self.target, self.window, apps).is_some()
    }
}

pub(super) fn freeze_association(
    broker: &Broker,
    targets: &BTreeMap<u64, Target>,
    peer: &Peer,
    registration: ControllerRegistrationId,
    enrollment: ControllerEnrollmentId,
    apps: &[&FesTermApp],
    guard: &festerm_session::AutomationAdmissionGuard,
) -> festerm_automation::Result<FrozenAssociation> {
    let record = peer
        .controller
        .registrations
        .values()
        .find(|record| record.internal == registration)
        .ok_or(festerm_automation::Error::Unauthorized)?;
    let association = record
        .association
        .filter(|association| association.enrollment == enrollment)
        .ok_or(festerm_automation::Error::Revoked)?;
    let frozen = FrozenAssociation {
        principal: peer.principal,
        registration,
        enrollment,
        public: association.public,
        target: association.target,
        window: association.window,
        valid: record.valid.clone(),
    };
    if !frozen.is_current(broker, targets, peer, apps, guard) {
        return Err(festerm_automation::Error::Revoked);
    }
    Ok(frozen)
}

enum Selection {
    Choosing {
        registration: ControllerRegistrationId,
        token: u64,
        window: WindowId,
    },
    Reviewing {
        registration: ControllerRegistrationId,
        token: u64,
        candidate: ControllerTargetRow,
        workspace_revision: u64,
        proposal: ControllerEnrollmentProposal,
    },
}

impl Selection {
    fn registration(&self) -> ControllerRegistrationId {
        match self {
            Self::Choosing { registration, .. } | Self::Reviewing { registration, .. } => {
                *registration
            }
        }
    }

    fn view(&self) -> ControllerSelectionView {
        match self {
            Self::Choosing { token, window, .. } => ControllerSelectionView::Choosing {
                token: *token,
                window: *window,
            },
            Self::Reviewing {
                token, candidate, ..
            } => ControllerSelectionView::Reviewing {
                token: *token,
                window: candidate.window,
                target: candidate.reference,
            },
        }
    }
}

struct MetadataReview {
    token: u64,
    candidate: ControllerTargetRow,
    review: presentation::PendingEnrollmentMetadataReview,
    valid: Rc<Cell<bool>>,
}

impl Drop for MetadataReview {
    fn drop(&mut self) {
        self.valid.set(false);
    }
}

pub(super) struct PendingDirectInput {
    pub(super) request: BackendRequest,
    pub(super) deadline: Instant,
    pub(super) valid: Rc<Cell<bool>>,
    pub(super) token: u64,
    association: FrozenAssociation,
    phase: DirectInputPhase,
}

impl PendingDirectInput {
    pub(super) fn next_deadline(&self) -> Instant {
        let capture = match &self.phase {
            DirectInputPhase::Requested => return self.deadline,
            DirectInputPhase::Preparing(capture) | DirectInputPhase::Reviewing { capture, .. } => {
                capture
            }
        };
        capture
            .sources
            .enrollments
            .iter()
            .map(|grant| grant.expires())
            .chain(capture.sources.metadata_expires)
            .fold(self.deadline, Instant::min)
    }
}

#[derive(Clone, PartialEq, Eq)]
struct DirectInputReviewCapture {
    candidate: ControllerTargetRow,
    workspace_revision: u64,
    sources: presentation::DirectInputSources,
    metadata_descriptions: Vec<String>,
}

enum DirectInputPhase {
    Requested,
    Preparing(DirectInputReviewCapture),
    Reviewing {
        capture: DirectInputReviewCapture,
        review: Box<presentation::PendingControlReview>,
    },
}

#[derive(Clone, Copy)]
pub(crate) struct DirectInputPreparation {
    pub(crate) channel: u64,
    pub(crate) token: u64,
    pub(crate) window: WindowId,
    pub(crate) target: TargetRef,
}

impl Drop for PendingDirectInput {
    fn drop(&mut self) {
        self.valid.set(false);
    }
}

#[derive(Default)]
pub(super) struct PeerState {
    registrations: BTreeMap<ControllerId, Registration>,
    issued_ids: BTreeSet<ControllerId>,
    selection: Option<Selection>,
    metadata_review: Option<MetadataReview>,
    metadata_revocations: BTreeSet<(ControllerRegistrationId, ControllerEnrollmentId)>,
    pub(super) direct_input: Option<PendingDirectInput>,
    direct_input_revision: u64,
    requests: usize,
    #[cfg(test)]
    next_test_id: Option<u128>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn enqueue_direct_input(
    broker: &Broker,
    targets: &BTreeMap<u64, Target>,
    peer: &mut Peer,
    request: BackendRequest,
    apps: &[&FesTermApp],
    privacy: &PrivacyGate,
    stop: &AtomicBool,
    qualified: bool,
    now: Instant,
) {
    let (handle, has_presentation_revision) = match &request.operation {
        Operation::TerminalSendText {
            enrollment_ref,
            presentation_revision,
            ..
        }
        | Operation::TerminalSendKeys {
            enrollment_ref,
            presentation_revision,
            ..
        } => (
            ControllerId::parse(enrollment_ref),
            presentation_revision.is_some(),
        ),
        _ => {
            finish(
                peer,
                &request.key,
                BackendResult::Refused(Refusal::InvalidArguments),
                now,
            );
            return;
        }
    };
    let preflight_refusal = if !qualified || has_presentation_revision || peer.grant.is_some() {
        Some(Refusal::Unavailable)
    } else if peer.controller.direct_input.is_some() {
        Some(Refusal::BudgetExceeded)
    } else {
        None
    };
    if let Some(reason) = preflight_refusal {
        finish(peer, &request.key, BackendResult::Refused(reason), now);
        return;
    }
    let Some(handle) = handle else {
        finish(
            peer,
            &request.key,
            BackendResult::Refused(Refusal::InvalidArguments),
            now,
        );
        return;
    };
    let association = match festerm_session::try_automation_admission_lock() {
        Ok(guard) if peer_is_open(peer, privacy, stop) => peer
            .controller
            .registrations
            .values()
            .find_map(|record| {
                let association = record.association.filter(|bound| bound.public == handle)?;
                Some((record.internal, association.enrollment))
            })
            .ok_or(Refusal::NotAuthorized)
            .and_then(|(registration, enrollment)| {
                freeze_association(
                    broker,
                    targets,
                    peer,
                    registration,
                    enrollment,
                    apps,
                    &guard,
                )
                .map_err(refusal)
            }),
        Ok(_) | Err(festerm_session::AutomationAdmissionTryError::Busy) => {
            Err(Refusal::Unavailable)
        }
        Err(festerm_session::AutomationAdmissionTryError::Poisoned) => {
            tracing::error!("automation admission lock poisoned; input review refused");
            Err(Refusal::Unavailable)
        }
    };
    let association = match association {
        Ok(association) => association,
        Err(reason) => {
            finish(peer, &request.key, BackendResult::Refused(reason), now);
            return;
        }
    };
    let Some(token) = peer.controller.direct_input_revision.checked_add(1) else {
        finish(
            peer,
            &request.key,
            BackendResult::Refused(Refusal::Unavailable),
            now,
        );
        return;
    };
    peer.controller.direct_input_revision = token;
    peer.controller.direct_input = Some(PendingDirectInput {
        request,
        deadline: now + festerm_automation::MAX_GRANT_LIFETIME,
        valid: Rc::new(Cell::new(true)),
        token,
        association,
        phase: DirectInputPhase::Requested,
    });
}

pub(super) fn cancel_direct_input(peer: &mut Peer, key: &RequestKey) -> bool {
    if peer
        .controller
        .direct_input
        .as_ref()
        .is_some_and(|input| &input.request.key == key)
    {
        peer.controller.direct_input = None;
        true
    } else {
        false
    }
}

pub(super) fn expire_direct_input(peer: &mut Peer, now: Instant) {
    let reason = peer.controller.direct_input.as_ref().and_then(|input| {
        if !input.valid.get() || !input.association.valid.load(Ordering::Acquire) {
            Some(Refusal::NotAuthorized)
        } else if now >= input.deadline {
            Some(Refusal::Unavailable)
        } else if now >= input.next_deadline() {
            Some(Refusal::NotAuthorized)
        } else {
            None
        }
    });
    if let Some(reason) = reason {
        let Some(input) = peer.controller.direct_input.take() else {
            return;
        };
        let key = input.request.key.clone();
        drop(input);
        finish(peer, &key, BackendResult::Refused(reason), now);
    }
}

struct Reply {
    result: BackendResult,
    flags: Vec<Arc<AtomicBool>>,
    targets: Vec<ipc::TargetGuard>,
}

impl Reply {
    fn plain(result: BackendResult) -> Self {
        Self {
            result,
            flags: Vec::new(),
            targets: Vec::new(),
        }
    }
}

pub(super) fn is_local_backend(session: &SessionTab) -> bool {
    let backend = session.controller.session();
    #[cfg(test)]
    if matches!(backend, Some(crate::tabs::ApplicationSession::TestLocal(_))) {
        return true;
    }
    matches!(backend, Some(crate::tabs::ApplicationSession::Local(_)))
}

// The caller holds the admission lock through a decision or response capture.
fn local_target_is_current(target: &Target, apps: &[&FesTermApp]) -> bool {
    let mut sessions = apps
        .iter()
        .filter_map(|app| app.automation_session(TabId::from_chip_id(target.reference.session())));
    let Some(session) = sessions.next() else {
        return false;
    };
    if sessions.next().is_some()
        || !target.local_pty
        || !is_local_backend(session)
        || session.controller.automation_instance() != Some(target.controller_instance)
        || session.controller.lifecycle_generation() != target.controller_generation
        || !matches!(
            session.controller.lifecycle(),
            Some(SessionLifecycle::Running)
        )
        || !Arc::ptr_eq(&session.controller.automation_liveness(), &target.liveness)
        || !session
            .controller
            .automation_attachment_epoch()
            .is_some_and(|attachment| Arc::ptr_eq(&attachment, &target.attachment))
    {
        return false;
    }
    // The backend-owned attachment epoch is the live witness, including ABA.
    target.guard().is_valid()
}

fn exact_target<'a>(
    targets: &'a BTreeMap<u64, Target>,
    reference: TargetRef,
    window: WindowId,
    apps: &[&FesTermApp],
) -> Option<&'a Target> {
    let target = targets.get(&reference.session())?;
    (target.reference == reference
        && local_target_is_current(target, apps)
        && apps.iter().any(|app| {
            app.automation_window() == window
                && app
                    .automation_session(TabId::from_chip_id(reference.session()))
                    .is_some()
        }))
    .then_some(target)
}

fn mint_id_with(
    peer: &mut Peer,
    mut generate: impl FnMut() -> Result<[u8; 16], Refusal>,
) -> Result<ControllerId, Refusal> {
    if peer.controller.issued_ids.len() >= MAX_ISSUED_IDS {
        return Err(Refusal::BudgetExceeded);
    }
    for _ in 0..MAX_ID_ATTEMPTS {
        let id = ControllerId::from_random_bytes(generate()?);
        if peer.controller.issued_ids.insert(id) {
            return Ok(id);
        }
    }
    Err(Refusal::Unavailable)
}

fn mint_id(peer: &mut Peer) -> Result<ControllerId, Refusal> {
    #[cfg(test)]
    if let Some(mut next) = peer.controller.next_test_id.take() {
        let result = mint_id_with(peer, || {
            let bytes = next.to_be_bytes();
            next = next.checked_add(1).ok_or(Refusal::Unavailable)?;
            Ok(bytes)
        });
        peer.controller.next_test_id = Some(next);
        return result;
    }
    mint_id_with(peer, random::<16>)
}

fn public_registration(peer: &Peer, internal: ControllerRegistrationId) -> Option<ControllerId> {
    peer.controller
        .registrations
        .iter()
        .find_map(|(id, record)| (record.internal == internal).then_some(*id))
}

// All record flags are changed under the admission lock.
fn revoke_associated_control(
    broker: &mut Broker,
    principal: ConnectionId,
    record: &mut Registration,
    guard: &festerm_session::AutomationAdmissionGuard,
) {
    if let Some((target, grant)) = record.direct_control.take() {
        if broker.native_control_grant(principal, target, Instant::now()) == Ok(grant) {
            broker.revoke_ordinary(principal, guard);
        }
    }
}

fn remove_locked(
    broker: &mut Broker,
    peer: &mut Peer,
    id: ControllerId,
    guard: &festerm_session::AutomationAdmissionGuard,
) -> bool {
    let Some(mut record) = peer.controller.registrations.remove(&id) else {
        return false;
    };
    record.valid.store(false, Ordering::Release);
    revoke_associated_control(broker, peer.principal, &mut record, guard);
    if peer
        .controller
        .selection
        .as_ref()
        .is_some_and(|selection| selection.registration() == record.internal)
    {
        peer.controller.selection = None;
    }
    if let Some(review) = peer
        .controller
        .metadata_review
        .as_ref()
        .filter(|review| review.review.registration() == record.internal)
    {
        review.valid.set(false);
    }
    if let Some(input) = peer
        .controller
        .direct_input
        .as_ref()
        .filter(|input| input.association.registration == record.internal)
    {
        input.valid.set(false);
    }
    let _ = broker.unregister_controller_session(peer.principal, record.internal, guard);
    true
}

fn status_locked(
    broker: &mut Broker,
    targets: &BTreeMap<u64, Target>,
    peer: &mut Peer,
    id: ControllerId,
    apps: &[&FesTermApp],
    guard: &festerm_session::AutomationAdmissionGuard,
) -> Result<Reply, Refusal> {
    let record = peer
        .controller
        .registrations
        .get(&id)
        .ok_or(Refusal::NotAuthorized)?;
    let association = record.association;
    let target = association.and_then(|association| {
        (broker
            .resolve_controller_enrollment(peer.principal, record.internal, association.enrollment)
            .ok()
            == Some(association.target))
        .then(|| exact_target(targets, association.target, association.window, apps))
        .flatten()
    });
    if !record.valid.load(Ordering::Acquire)
        || broker
            .controller_registration(peer.principal, record.internal)
            .is_err()
        || (association.is_some() && target.is_none())
    {
        remove_locked(broker, peer, id, guard);
        return Err(Refusal::NotAuthorized);
    }
    Ok(Reply {
        result: BackendResult::ControllerStatus {
            registration_id: id,
            association_id: association.map(|association| association.public),
        },
        flags: vec![record.valid.clone()],
        targets: target.map(Target::guard).into_iter().collect(),
    })
}

pub(super) fn is_operation(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::ControllerCapabilities
            | Operation::PresentationCapabilities
            | Operation::ControllerRegister { .. }
            | Operation::ControllerStatus { .. }
            | Operation::ControllerUnregister { .. }
    )
}

struct MetadataDisclosure {
    data: serde_json::Value,
    expires: Instant,
    liveness: festerm_automation::DisclosureLiveness,
    target: ipc::TargetGuard,
    association: Arc<AtomicBool>,
}

#[allow(clippy::too_many_arguments)]
fn prepare_metadata_disclosure(
    broker: &mut Broker,
    targets: &BTreeMap<u64, Target>,
    metadata: EnrollmentMetadataContext<'_>,
    peer: &mut Peer,
    session_ref: String,
    apps: &[&FesTermApp],
    guard: &festerm_session::AutomationAdmissionGuard,
) -> Result<MetadataDisclosure, Refusal> {
    let handle = ControllerId::parse(&session_ref).ok_or(Refusal::NotAuthorized)?;
    let bound = metadata
        .grants
        .iter()
        .find(|((principal, _, _), bound)| {
            *principal == peer.principal && bound.matches_handle(handle)
        })
        .map(|(_, bound)| bound)
        .ok_or(Refusal::NotAuthorized)?;
    let now = Instant::now();
    if !bound.is_current(broker, targets, peer, apps, now, guard) {
        return Err(Refusal::NotAuthorized);
    }

    let target = bound.grant.target();
    let binding = direct_binding(&Operation::SessionsGet { session_ref }, Some(target))?;
    let run = broker
        .admit_enrollment_metadata_run(
            peer.principal,
            bound.grant.registration(),
            bound.grant.enrollment(),
            MetadataRunSpec {
                parameters: binding.parameters,
                targets: BTreeSet::from([target]),
                budget: Budget::default(),
                deadline: bound.grant.expires(),
            },
            now,
        )
        .map_err(refusal)?;
    retain_completion(peer, run, None);
    let ticket = broker
        .prepare_request(
            peer.principal,
            run,
            AuthorityOperation::Discover {
                max_bytes: MAX_ENROLLMENT_METADATA_BYTES,
            },
            Instant::now(),
        )
        .map_err(refusal)?;
    peer.pending_completion
        .as_mut()
        .expect("metadata run retains its completion")
        .request = Some(ticket);
    let visible = broker
        .discovery_targets(peer.principal, ticket, Instant::now())
        .map_err(refusal)?;
    if visible.as_slice() != [target] {
        return Err(Refusal::NotAuthorized);
    }
    let session = session(apps, target).ok_or(Refusal::StaleTarget)?;
    let data = observation::bounded(
        serde_json::json!({
            "sessions": [{
                "session_ref": handle.to_wire(),
                "label": session.label.chars().take(128).collect::<String>(),
                "lifecycle": "running",
                "untrusted": true,
            }],
            "untrusted": true,
        }),
        MAX_ENROLLMENT_METADATA_BYTES as usize,
    )?;
    let encoded = serde_json::to_vec(&data).map_err(|_| Refusal::InvalidBackendResult)?;
    let encoded_bytes = u32::try_from(encoded.len()).map_err(|_| Refusal::ResponseTooLarge)?;
    let now = Instant::now();
    broker
        .commit_disclosure(peer.principal, ticket, encoded_bytes, now)
        .map_err(refusal)?;
    let liveness = broker
        .disclosure_liveness(peer.principal, ticket, now)
        .map_err(refusal)?;
    let (target, association) = bound.delivery_guards();
    Ok(MetadataDisclosure {
        data,
        expires: bound.grant.expires(),
        liveness,
        target,
        association,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_metadata(
    broker: &mut Broker,
    targets: &BTreeMap<u64, Target>,
    metadata: EnrollmentMetadataContext<'_>,
    peer: &mut Peer,
    request: BackendRequest,
    apps: &[&FesTermApp],
    privacy: &PrivacyGate,
    stop: &AtomicBool,
) {
    let Operation::SessionsGet { session_ref } = request.operation else {
        finish(
            peer,
            &request.key,
            BackendResult::Refused(Refusal::InvalidArguments),
            Instant::now(),
        );
        return;
    };
    let result = if let Some(guard) = festerm_session::automation_admission_lock() {
        if metadata.qualified && peer_is_open(peer, privacy, stop) {
            prepare_metadata_disclosure(broker, targets, metadata, peer, session_ref, apps, &guard)
        } else {
            Err(Refusal::Unavailable)
        }
    } else {
        peer.connection.close();
        return;
    };
    match result {
        Ok(disclosure) => {
            if let Some(response) = peer.machine.complete(
                &request.key,
                BackendResult::AuthorizedObservation(disclosure.data),
            ) {
                if peer
                    .connection
                    .send_disclosure(
                        response.into_bytes(),
                        disclosure.expires,
                        &[disclosure.target],
                        &[disclosure.association],
                        (peer.principal, disclosure.liveness),
                    )
                    .is_err()
                {
                    peer.connection.close();
                    tracing::warn!("automation metadata delivery admission failed");
                }
            }
        }
        Err(error) => finish(
            peer,
            &request.key,
            BackendResult::Refused(error),
            Instant::now(),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execute(
    broker: &mut Broker,
    targets: &BTreeMap<u64, Target>,
    peer: &mut Peer,
    request: BackendRequest,
    apps: &[&FesTermApp],
    privacy: &PrivacyGate,
    stop: &AtomicBool,
    qualified: bool,
    metadata_qualified: bool,
    now: Instant,
) {
    if peer.controller.requests >= MAX_CONTROLLER_REQUESTS {
        if let Some(guard) = festerm_session::automation_admission_lock() {
            peer.connection.close_under_admission(&guard);
            broker.disconnect(peer.principal, &guard);
        } else {
            peer.connection.closed_flag().store(true, Ordering::Release);
            tracing::error!("automation admission lock poisoned; disclosure remains closed");
        }
        peer.machine.close();
        peer.controller = PeerState::default();
        return;
    }
    peer.controller.requests += 1;
    let key = request.key;
    let result = if let Some(guard) = festerm_session::automation_admission_lock() {
        let open =
            !privacy.is_closed() && !stop.load(Ordering::Acquire) && !peer.connection.is_closed();
        match request.operation {
            Operation::ControllerCapabilities | Operation::PresentationCapabilities => {
                Ok(Reply::plain(BackendResult::ControllerCapabilities {
                    enrollment_available: qualified && open,
                    metadata_available: metadata_qualified && qualified && open,
                }))
            }
            _ if !open || !qualified => Err(Refusal::Unavailable),
            Operation::ControllerRegister { local_id } => {
                match broker.register_controller_session(
                    peer.principal,
                    local_id.as_bytes(),
                    &guard,
                ) {
                    Ok(internal) => {
                        let existing = public_registration(peer, internal);
                        let id = match existing {
                            Some(id) => Ok(id),
                            None => mint_id(peer).inspect_err(|_| {
                                let _ = broker.unregister_controller_session(
                                    peer.principal,
                                    internal,
                                    &guard,
                                );
                            }),
                        };
                        id.and_then(|id| {
                            if existing.is_none() {
                                peer.controller.registrations.insert(
                                    id,
                                    Registration {
                                        internal,
                                        valid: Arc::new(AtomicBool::new(true)),
                                        association: None,
                                        direct_control: None,
                                    },
                                );
                            }
                            status_locked(broker, targets, peer, id, apps, &guard)
                        })
                    }
                    Err(error) => Err(refusal(error)),
                }
            }
            Operation::ControllerStatus { registration_id } => {
                status_locked(broker, targets, peer, registration_id, apps, &guard)
            }
            Operation::ControllerUnregister { registration_id } => {
                if remove_locked(broker, peer, registration_id, &guard) {
                    Ok(Reply::plain(BackendResult::ControllerUnregistered {
                        registration_id,
                    }))
                } else {
                    Err(Refusal::NotAuthorized)
                }
            }
            _ => Err(Refusal::InvalidArguments),
        }
    } else {
        Err(Refusal::Unavailable)
    };
    let reply = result.unwrap_or_else(|error| Reply::plain(BackendResult::Refused(error)));
    if let Some(response) = peer.machine.complete(&key, reply.result) {
        if peer
            .connection
            .send_guarded(
                response.into_bytes(),
                now + Duration::from_secs(2),
                &reply.targets,
                &reply.flags,
            )
            .is_err()
        {
            peer.connection.close();
        }
    }
}

pub(super) fn reconcile(
    broker: &mut Broker,
    targets: &BTreeMap<u64, Target>,
    peers: &mut BTreeMap<u64, Peer>,
    apps: &[&FesTermApp],
    workspace_revision: Option<u64>,
    guard: &festerm_session::AutomationAdmissionGuard,
) {
    if peers.is_empty() {
        return;
    }
    let (candidates, incomplete) = local_candidates(targets, apps);
    for peer in peers.values_mut() {
        let stale: Vec<_> = peer
            .controller
            .registrations
            .iter()
            .filter(|(_, record)| {
                !record.valid.load(Ordering::Acquire)
                    || broker
                        .controller_registration(peer.principal, record.internal)
                        .is_err()
                    || record.association.is_some_and(|association| {
                        broker
                            .resolve_controller_enrollment(
                                peer.principal,
                                record.internal,
                                association.enrollment,
                            )
                            .ok()
                            != Some(association.target)
                            || exact_target(targets, association.target, association.window, apps)
                                .is_none()
                    })
            })
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            remove_locked(broker, peer, id, guard);
        }
        if peer.controller.selection.as_ref().is_some_and(|selection| {
            public_registration(peer, selection.registration()).is_none()
                || match selection {
                    Selection::Choosing { window, .. } => {
                        !apps.iter().any(|app| app.automation_window() == *window)
                    }
                    Selection::Reviewing {
                        candidate,
                        workspace_revision: selected_revision,
                        ..
                    } => {
                        incomplete
                            || workspace_revision != Some(*selected_revision)
                            || !candidates.contains(candidate)
                    }
                }
        }) {
            peer.controller.selection = None;
        }
        if let Some(review) = peer.controller.metadata_review.as_ref() {
            if incomplete || !candidates.contains(&review.candidate) {
                review.valid.set(false);
            }
        }
    }
}

fn next_selection(revision: &mut u64) -> Result<u64, Refusal> {
    *revision = revision.checked_add(1).ok_or(Refusal::Unavailable)?;
    Ok(*revision)
}

fn peer_is_open(peer: &Peer, privacy: &PrivacyGate, stop: &AtomicBool) -> bool {
    !peer.connection.is_closed() && !privacy.is_closed() && !stop.load(Ordering::Acquire)
}

pub(super) const BUSY_STATUS: &str =
    "Controller action refused while automation admission is busy. Try again.";
// The deferred tick can also represent startup or cancellation safety work.
pub(super) const RECONCILIATION_PENDING_STATUS: &str =
    "Controller action refused while automation admission or safety work is pending. Try again.";
pub(super) const ACCEPTED_STATUS: &str =
    "Controller action accepted. This action granted no terminal access.";

#[derive(Debug, PartialEq, Eq)]
pub(super) enum CommandError {
    Busy,
    Refused(Refusal),
}

fn input_source_description(
    targets: &BTreeMap<u64, Target>,
    apps: &[&FesTermApp],
    reference: TargetRef,
    _guard: &festerm_session::AutomationAdmissionGuard,
) -> Option<String> {
    let target = targets
        .get(&reference.session())
        .filter(|target| target.reference == reference && target.guard().is_valid())?;
    let mut matching = apps.iter().enumerate().filter_map(|(window, app)| {
        let session = app.automation_session(TabId::from_chip_id(reference.session()))?;
        Some((window, *app, session))
    });
    let (window, app, session) = matching.next()?;
    if matching.next().is_some()
        || session.controller.automation_instance() != Some(target.controller_instance)
        || session.controller.lifecycle_generation() != target.controller_generation
        || !matches!(
            session.controller.lifecycle(),
            Some(SessionLifecycle::Running)
        )
        || !Arc::ptr_eq(&session.controller.automation_liveness(), &target.liveness)
        || !session
            .controller
            .automation_attachment_epoch()
            .is_some_and(|attachment| Arc::ptr_eq(&attachment, &target.attachment))
    {
        return None;
    }
    let (position, active) =
        app.automation_session_context(TabId::from_chip_id(reference.session()))?;
    Some(format!(
        "Window {}, tab position {}{}; untrusted name \"{}\" (session {}, attachment {})",
        window + 1,
        position,
        if active { ", active" } else { "" },
        quoted_approval_label(&session.label),
        reference.session(),
        reference.generation(),
    ))
}

fn capture_direct_input_review(
    coordinator: &Coordinator,
    channel: u64,
    window: WindowId,
    input: &PendingDirectInput,
    apps: &[&FesTermApp],
    guard: &festerm_session::AutomationAdmissionGuard,
) -> festerm_automation::Result<DirectInputReviewCapture> {
    let now = Instant::now();
    let peer = coordinator
        .peers
        .get(&channel)
        .ok_or(festerm_automation::Error::Closed)?;
    let broker = coordinator
        .broker
        .as_ref()
        .ok_or(festerm_automation::Error::Unavailable)?;
    if !coordinator.direct_input_qualified()
        || !coordinator.controller_qualified()
        || !coordinator.controller_opted_in
        || peer.grant.is_some()
        || !peer_is_open(peer, &coordinator.privacy_gate, &coordinator.stop)
        || !input.valid.get()
        || now >= input.deadline
        || input.association.window != window
        || !input
            .association
            .is_current(broker, &coordinator.targets, peer, apps, guard)
    {
        return Err(festerm_automation::Error::Revoked);
    }
    let (candidates, incomplete) = local_candidates(&coordinator.targets, apps);
    let candidate = candidates
        .into_iter()
        .find(|candidate| {
            candidate.reference == input.association.target && candidate.window == window
        })
        .filter(|_| !incomplete)
        .ok_or(festerm_automation::Error::StaleTarget)?;
    let workspace_revision = coordinator
        .presentation_workspace
        .as_ref()
        .and_then(WorkspaceRevision::current)
        .ok_or(festerm_automation::Error::Unavailable)?;
    let sources = coordinator.direct_input_sources_locked(channel, now, guard)?;
    let mut metadata_descriptions = Vec::new();
    for target in &sources.metadata_targets {
        let description = input_source_description(&coordinator.targets, apps, *target, guard)
            .ok_or(festerm_automation::Error::StaleTarget)?;
        metadata_descriptions.push(format!("Separate metadata grant: {description}"));
    }
    for grant in &sources.enrollments {
        let (public, record) = peer
            .controller
            .registrations
            .iter()
            .find(|(_, record)| record.internal == grant.registration())
            .ok_or(festerm_automation::Error::Revoked)?;
        let association = record
            .association
            .filter(|association| {
                association.enrollment == grant.enrollment() && association.target == grant.target()
            })
            .filter(|_| record.valid.load(Ordering::Acquire))
            .ok_or(festerm_automation::Error::Revoked)?;
        let identifier = broker.controller_registration(peer.principal, record.internal)?;
        let identifier = std::str::from_utf8(identifier)
            .map_err(|_| festerm_automation::Error::InvalidRequest)?;
        let description =
            input_source_description(&coordinator.targets, apps, grant.target(), guard)
                .ok_or(festerm_automation::Error::StaleTarget)?;
        metadata_descriptions.push(format!(
            "Enrollment {} (association {}), untrusted identifier \"{}\": {}",
            public.to_wire(),
            association.public.to_wire(),
            quoted_approval_label(identifier),
            description,
        ));
    }
    Ok(DirectInputReviewCapture {
        candidate,
        workspace_revision,
        sources,
        metadata_descriptions,
    })
}

pub(super) fn command(
    coordinator: &mut Coordinator,
    window: WindowId,
    action: NativeCommand,
    apps: &[&FesTermApp],
) -> Result<(), CommandError> {
    if matches!(
        action,
        NativeCommand::ReviewControllerInput { .. } | NativeCommand::CancelControllerInput { .. }
    ) {
        return direct_input_command(coordinator, window, action, apps);
    }
    if matches!(
        action,
        NativeCommand::ReviewControllerMetadata { .. }
            | NativeCommand::ConfirmControllerMetadata { .. }
            | NativeCommand::CancelControllerMetadata { .. }
            | NativeCommand::RevokeControllerMetadata { .. }
    ) {
        return metadata_command(coordinator, window, action, apps);
    }
    if let NativeCommand::CancelController {
        channel,
        registration,
    } = &action
    {
        let peer = coordinator
            .peers
            .get_mut(channel)
            .ok_or(CommandError::Refused(Refusal::Closed))?;
        if peer
            .controller
            .selection
            .as_ref()
            .is_some_and(|pending| pending.registration() == *registration)
        {
            peer.controller.selection = None;
        }
        return Ok(());
    }
    let guard = if matches!(&action, NativeCommand::UnregisterController { .. }) {
        festerm_session::automation_admission_lock().ok_or_else(|| {
            tracing::error!("automation admission lock poisoned; controller action refused");
            CommandError::Refused(Refusal::Unavailable)
        })?
    } else {
        festerm_session::try_automation_admission_lock().map_err(|error| match error {
            festerm_session::AutomationAdmissionTryError::Busy => CommandError::Busy,
            festerm_session::AutomationAdmissionTryError::Poisoned => {
                tracing::error!("automation admission lock poisoned; controller action refused");
                CommandError::Refused(Refusal::Unavailable)
            }
        })?
    };
    command_under_admission(coordinator, window, action, apps, &guard)
        .map_err(CommandError::Refused)
}

fn direct_input_command(
    coordinator: &mut Coordinator,
    window: WindowId,
    action: NativeCommand,
    apps: &[&FesTermApp],
) -> Result<(), CommandError> {
    match action {
        NativeCommand::CancelControllerInput {
            channel,
            registration,
            input,
        } => {
            let pending = coordinator
                .peers
                .get(&channel)
                .and_then(|peer| peer.controller.direct_input.as_ref())
                .filter(|pending| {
                    pending.token == input && pending.association.registration == registration
                })
                .ok_or(CommandError::Refused(Refusal::NotAuthorized))?;
            pending.valid.set(false);
            coordinator.status = "Direct input cancelled; no input was admitted.";
            Ok(())
        }
        NativeCommand::ReviewControllerInput {
            channel,
            registration,
            input,
        } => {
            let guard = try_metadata_admission()?;
            let pending = coordinator
                .peers
                .get(&channel)
                .and_then(|peer| peer.controller.direct_input.as_ref())
                .filter(|pending| {
                    pending.token == input
                        && pending.association.registration == registration
                        && matches!(&pending.phase, DirectInputPhase::Requested)
                })
                .ok_or(CommandError::Refused(Refusal::NotAuthorized))?;
            let capture =
                capture_direct_input_review(coordinator, channel, window, pending, apps, &guard)
                    .map_err(metadata_command_error)?;
            drop(guard);
            let pending = coordinator
                .peers
                .get_mut(&channel)
                .and_then(|peer| peer.controller.direct_input.as_mut())
                .filter(|pending| pending.token == input && pending.valid.get())
                .ok_or(CommandError::Refused(Refusal::NotAuthorized))?;
            pending.phase = DirectInputPhase::Preparing(capture);
            coordinator.status = "Preparing the exact input and metadata disclosure for review.";
            Ok(())
        }
        _ => Err(CommandError::Refused(Refusal::InvalidArguments)),
    }
}

pub(super) fn direct_input_preparations(coordinator: &Coordinator) -> Vec<DirectInputPreparation> {
    if coordinator.reconciliation_deferred
        || coordinator.stop.load(Ordering::Acquire)
        || coordinator.privacy_gate.is_closed()
    {
        return Vec::new();
    }
    coordinator
        .peers
        .iter()
        .filter_map(|(channel, peer)| {
            let pending = peer.controller.direct_input.as_ref()?;
            if peer.connection.is_closed()
                || !pending.valid.get()
                || Instant::now() >= pending.deadline
                || !matches!(&pending.phase, DirectInputPhase::Preparing(_))
            {
                return None;
            }
            Some(DirectInputPreparation {
                channel: *channel,
                token: pending.token,
                window: pending.association.window,
                target: pending.association.target,
            })
        })
        .collect()
}

pub(super) fn prepare_direct_input_review(
    coordinator: &mut Coordinator,
    preparation: DirectInputPreparation,
    tab: &mut SessionTab,
) -> festerm_automation::Result<()> {
    use festerm_automation::Error;
    let pending = coordinator
        .peers
        .get(&preparation.channel)
        .and_then(|peer| peer.controller.direct_input.as_ref())
        .filter(|pending| pending.token == preparation.token && pending.valid.get())
        .ok_or(Error::Revoked)?;
    let DirectInputPhase::Preparing(capture) = &pending.phase else {
        return Err(Error::Revoked);
    };
    let capture = capture.clone();
    let association = pending.association.clone();
    let valid = pending.valid.clone();
    let deadline = pending.deadline;
    let operation = pending.request.operation.clone();
    let now = Instant::now();
    if now >= deadline
        || coordinator
            .presentation_workspace
            .as_ref()
            .and_then(WorkspaceRevision::current)
            != Some(capture.workspace_revision)
    {
        return Err(Error::Revoked);
    }
    let guard = festerm_session::try_automation_admission_lock().map_err(|error| match error {
        festerm_session::AutomationAdmissionTryError::Busy => Error::Busy,
        festerm_session::AutomationAdmissionTryError::Poisoned => Error::Unavailable,
    })?;
    let peer = coordinator
        .peers
        .get(&preparation.channel)
        .ok_or(Error::Closed)?;
    let broker = coordinator.broker.as_ref().ok_or(Error::Unavailable)?;
    if !peer_is_open(peer, &coordinator.privacy_gate, &coordinator.stop)
        || !association.is_bound(broker, &coordinator.targets, peer, &guard)
        || association.window != preparation.window
        || association.target != preparation.target
    {
        return Err(Error::Revoked);
    }
    drop(guard);
    let action = match &operation {
        Operation::TerminalSendText { text, submit, .. } => presentation::DirectAction::Text {
            text,
            submit: *submit,
        },
        Operation::TerminalSendKeys { keys, .. } => presentation::DirectAction::Keys(keys),
        _ => return Err(Error::InvalidRequest),
    };
    let review = coordinator.prepare_direct_action_review_with_sources(
        preparation.channel,
        preparation.target,
        action,
        deadline.saturating_duration_since(now),
        tab,
        &capture.sources.scope,
    )?;
    if let Err(error) =
        coordinator.bind_direct_request_review(&review, association, valid.clone(), deadline)
    {
        valid.set(false);
        if let Err(cleanup) =
            coordinator.cancel_control_handoff_review(preparation.channel, &review)
        {
            if let Some(peer) = coordinator.peers.get(&preparation.channel) {
                peer.connection.close();
            }
            tracing::warn!(?cleanup, "direct input review cleanup was refused");
        }
        return Err(error);
    }
    let mut enrollments = review.enrollment_metadata_grants().to_vec();
    enrollments.sort_unstable();
    if !valid.get()
        || review.metadata_grant() != capture.sources.metadata
        || enrollments != capture.sources.enrollments
        || review.metadata_sources() != &capture.sources.scope.discover
        || review.direct_action_display().is_none()
    {
        valid.set(false);
        return Err(Error::Revoked);
    }
    let pending = coordinator
        .peers
        .get_mut(&preparation.channel)
        .and_then(|peer| peer.controller.direct_input.as_mut())
        .filter(|pending| pending.token == preparation.token && pending.valid.get())
        .ok_or(Error::Revoked)?;
    pending.phase = DirectInputPhase::Reviewing {
        capture,
        review: Box::new(review),
    };
    coordinator.status = "Review the complete input and metadata disclosure before approving.";
    Ok(())
}

pub(super) fn refuse_direct_input_preparation(
    coordinator: &mut Coordinator,
    preparation: DirectInputPreparation,
    reason: Refusal,
) {
    let Some(peer) = coordinator.peers.get_mut(&preparation.channel) else {
        return;
    };
    if peer
        .controller
        .direct_input
        .as_ref()
        .is_some_and(|pending| pending.token == preparation.token)
    {
        if let Some(pending) = peer.controller.direct_input.take() {
            let key = pending.request.key.clone();
            drop(pending);
            finish(peer, &key, BackendResult::Refused(reason), Instant::now());
        }
    }
}

fn metadata_command(
    coordinator: &mut Coordinator,
    window: WindowId,
    action: NativeCommand,
    apps: &[&FesTermApp],
) -> Result<(), CommandError> {
    match action {
        NativeCommand::CancelControllerMetadata {
            channel,
            registration,
            review,
        } => {
            let peer = coordinator
                .peers
                .get_mut(&channel)
                .ok_or(CommandError::Refused(Refusal::Closed))?;
            let pending = peer
                .controller
                .metadata_review
                .as_ref()
                .filter(|pending| {
                    pending.token == review && pending.review.registration() == registration
                })
                .ok_or(CommandError::Refused(Refusal::NotAuthorized))?;
            pending.valid.set(false);
            coordinator.status = "Metadata review cancelled; cleanup is pending.";
            Ok(())
        }
        NativeCommand::RevokeControllerMetadata {
            channel,
            registration,
            enrollment,
        } => {
            let peer = coordinator
                .peers
                .get_mut(&channel)
                .ok_or(CommandError::Refused(Refusal::Closed))?;
            let current = peer.controller.registrations.values().any(|record| {
                record.internal == registration
                    && record
                        .association
                        .is_some_and(|association| association.enrollment == enrollment)
            });
            if !current {
                return Err(CommandError::Refused(Refusal::NotAuthorized));
            }
            if peer.controller.metadata_revocations.len() >= MAX_ISSUED_IDS {
                return Err(CommandError::Refused(Refusal::BudgetExceeded));
            }
            if let Some(pending) = peer.controller.metadata_review.as_ref().filter(|pending| {
                pending.review.registration() == registration
                    && pending.review.enrollment() == enrollment
            }) {
                pending.valid.set(false);
            }
            peer.controller
                .metadata_revocations
                .insert((registration, enrollment));
            coordinator.status = "Metadata revocation queued; no metadata delivery is available.";
            Ok(())
        }
        NativeCommand::ReviewControllerMetadata {
            channel,
            registration,
            enrollment,
        } => {
            if !coordinator.enrollment_metadata_ui_qualified() {
                return Err(CommandError::Refused(Refusal::Unavailable));
            }
            let peer = coordinator
                .peers
                .get(&channel)
                .ok_or(CommandError::Refused(Refusal::Closed))?;
            let association = peer
                .controller
                .registrations
                .values()
                .find(|record| record.internal == registration)
                .and_then(|record| record.association)
                .filter(|association| {
                    association.enrollment == enrollment && association.window == window
                })
                .ok_or(CommandError::Refused(Refusal::NotAuthorized))?;
            if peer.controller.metadata_review.is_some()
                || peer
                    .controller
                    .metadata_revocations
                    .contains(&(registration, enrollment))
            {
                return Err(CommandError::Refused(Refusal::Unavailable));
            }
            let token = next_selection(&mut coordinator.controller_selection_revision)
                .map_err(CommandError::Refused)?;
            let guard = try_metadata_admission()?;
            let (candidates, incomplete) = local_candidates(&coordinator.targets, apps);
            let candidate = candidates
                .into_iter()
                .find(|candidate| {
                    candidate.reference == association.target && candidate.window == window
                })
                .filter(|_| !incomplete)
                .ok_or(CommandError::Refused(Refusal::NotAuthorized))?;
            let review = coordinator
                .prepare_enrollment_metadata_review_locked(
                    channel,
                    registration,
                    enrollment,
                    festerm_automation::MAX_GRANT_LIFETIME,
                    apps,
                    &guard,
                )
                .map_err(metadata_command_error)?;
            let current = review.target() == association.target;
            drop(guard);
            coordinator
                .peers
                .get_mut(&channel)
                .ok_or(CommandError::Refused(Refusal::Closed))?
                .controller
                .metadata_review = Some(MetadataReview {
                token,
                candidate,
                review,
                valid: Rc::new(Cell::new(current)),
            });
            if !current {
                return Err(CommandError::Refused(Refusal::NotAuthorized));
            }
            coordinator.status = "Review this exact metadata grant before confirming.";
            Ok(())
        }
        NativeCommand::ConfirmControllerMetadata {
            channel,
            registration,
            review,
        } => {
            if !coordinator.enrollment_metadata_ui_qualified() {
                return Err(CommandError::Refused(Refusal::Unavailable));
            }
            let peer = coordinator
                .peers
                .get(&channel)
                .ok_or(CommandError::Refused(Refusal::Closed))?;
            let pending = peer
                .controller
                .metadata_review
                .as_ref()
                .filter(|pending| {
                    pending.token == review
                        && pending.candidate.window == window
                        && pending.review.registration() == registration
                        && pending.valid.get()
                        && !peer
                            .controller
                            .metadata_revocations
                            .contains(&(registration, pending.review.enrollment()))
                })
                .ok_or(CommandError::Refused(Refusal::NotAuthorized))?;
            let decision = pending.review.accept_from_native();
            let pending_review = pending.review.clone();
            let valid = pending.valid.clone();
            let candidate = pending.candidate.clone();
            let guard = try_metadata_admission()?;
            let (candidates, incomplete) = local_candidates(&coordinator.targets, apps);
            if incomplete || !candidates.contains(&candidate) || !valid.get() {
                valid.set(false);
                return Err(CommandError::Refused(Refusal::NotAuthorized));
            }
            let result = coordinator.approve_enrollment_metadata_review_locked(
                channel,
                &pending_review,
                &decision,
                apps,
                &guard,
            );
            drop(guard);
            match result {
                Ok(()) => {
                    valid.set(false);
                    if let Some(peer) = coordinator.peers.get_mut(&channel) {
                        peer.controller.metadata_review = None;
                    }
                    coordinator.status =
                        "Metadata grant approved; metadata delivery remains unavailable.";
                    Ok(())
                }
                Err(festerm_automation::Error::Busy) => Err(CommandError::Busy),
                Err(error) => {
                    valid.set(false);
                    Err(metadata_command_error(error))
                }
            }
        }
        _ => Err(CommandError::Refused(Refusal::InvalidArguments)),
    }
}

fn metadata_command_error(error: festerm_automation::Error) -> CommandError {
    if error == festerm_automation::Error::Busy {
        CommandError::Busy
    } else {
        CommandError::Refused(refusal(error))
    }
}

fn try_metadata_admission() -> Result<festerm_session::AutomationAdmissionGuard, CommandError> {
    festerm_session::try_automation_admission_lock().map_err(|error| match error {
        festerm_session::AutomationAdmissionTryError::Busy => CommandError::Busy,
        festerm_session::AutomationAdmissionTryError::Poisoned => {
            CommandError::Refused(Refusal::Unavailable)
        }
    })
}

fn command_under_admission(
    coordinator: &mut Coordinator,
    window: WindowId,
    action: NativeCommand,
    apps: &[&FesTermApp],
    guard: &festerm_session::AutomationAdmissionGuard,
) -> Result<(), Refusal> {
    let qualified = coordinator.controller_qualified();
    let available = qualified && coordinator.controller_opted_in;
    let workspace_revision = coordinator
        .presentation_workspace
        .as_ref()
        .and_then(WorkspaceRevision::current);
    let broker = coordinator.broker.as_mut().ok_or(Refusal::Unavailable)?;
    match action {
        NativeCommand::EnableControllerEnrollment => {
            if !qualified
                || coordinator.privacy_gate.is_closed()
                || coordinator.stop.load(Ordering::Acquire)
                || !apps.iter().any(|app| app.automation_window() == window)
            {
                return Err(Refusal::Unavailable);
            }
            coordinator.controller_opted_in = true;
            coordinator.status = "Controller registration opted in for new qualified connections; no terminal access was granted.";
        }
        NativeCommand::ChooseController {
            channel,
            registration,
        } => {
            let peer = coordinator.peers.get_mut(&channel).ok_or(Refusal::Closed)?;
            peer.controller.selection = None;
            let id = public_registration(peer, registration).ok_or(Refusal::NotAuthorized)?;
            if !available
                || !peer_is_open(peer, &coordinator.privacy_gate, &coordinator.stop)
                || !apps.iter().any(|app| app.automation_window() == window)
                || !peer.controller.registrations[&id]
                    .valid
                    .load(Ordering::Acquire)
                || broker
                    .controller_registration(peer.principal, registration)
                    .is_err()
            {
                return Err(Refusal::NotAuthorized);
            }
            let token = next_selection(&mut coordinator.controller_selection_revision)?;
            peer.controller.selection = Some(Selection::Choosing {
                registration,
                token,
                window,
            });
        }
        NativeCommand::SelectControllerTarget {
            channel,
            registration,
            selection,
            target,
        } => {
            let (candidates, incomplete) = local_candidates(&coordinator.targets, apps);
            let candidate = candidates
                .into_iter()
                .find(|candidate| candidate.reference == target && candidate.window == window);
            let peer = coordinator.peers.get_mut(&channel).ok_or(Refusal::Closed)?;
            let Some(id) = public_registration(peer, registration) else {
                peer.controller.selection = None;
                return Err(Refusal::NotAuthorized);
            };
            let Some(workspace_revision) = workspace_revision else {
                peer.controller.selection = None;
                return Err(Refusal::Unavailable);
            };
            let Some(candidate) = candidate else {
                peer.controller.selection = None;
                return Err(Refusal::NotAuthorized);
            };
            let selected_from_current_chooser = matches!(
                peer.controller.selection,
                Some(Selection::Choosing { registration: current, token, window: chooser })
                    if current == registration && token == selection && chooser == window
            );
            if incomplete
                || !available
                || !peer_is_open(peer, &coordinator.privacy_gate, &coordinator.stop)
                || !peer.controller.registrations[&id]
                    .valid
                    .load(Ordering::Acquire)
                || !selected_from_current_chooser
                || broker
                    .controller_registration(peer.principal, registration)
                    .is_err()
                || exact_target(&coordinator.targets, target, window, apps).is_none()
            {
                peer.controller.selection = None;
                return Err(Refusal::NotAuthorized);
            }
            peer.controller.selection = None;
            let token = next_selection(&mut coordinator.controller_selection_revision)?;
            let proposal = broker
                .prepare_controller_enrollment(peer.principal, registration, target, guard)
                .map_err(refusal)?;
            peer.controller.selection = Some(Selection::Reviewing {
                registration,
                token,
                candidate,
                workspace_revision,
                proposal,
            });
        }
        NativeCommand::ConfirmController {
            channel,
            registration,
            selection,
        } => {
            let (candidates, incomplete) = local_candidates(&coordinator.targets, apps);
            let peer = coordinator.peers.get_mut(&channel).ok_or(Refusal::Closed)?;
            let id = public_registration(peer, registration).ok_or(Refusal::NotAuthorized)?;
            let (candidate, selected_revision) = match peer.controller.selection.as_ref() {
                Some(Selection::Reviewing {
                    registration: current,
                    token,
                    candidate,
                    workspace_revision,
                    ..
                }) if *current == registration && *token == selection => {
                    (candidate, *workspace_revision)
                }
                _ => return Err(Refusal::NotAuthorized),
            };
            if incomplete
                || !available
                || !peer_is_open(peer, &coordinator.privacy_gate, &coordinator.stop)
                || !peer.controller.registrations[&id]
                    .valid
                    .load(Ordering::Acquire)
                || broker
                    .controller_registration(peer.principal, registration)
                    .is_err()
                || workspace_revision != Some(selected_revision)
                || candidate.window != window
                || !candidates.contains(candidate)
                || exact_target(&coordinator.targets, candidate.reference, window, apps).is_none()
            {
                peer.controller.selection = None;
                return Err(Refusal::NotAuthorized);
            }
            let target = candidate.reference;
            let public = mint_id(peer)?;
            let Some(Selection::Reviewing { proposal, .. }) = peer.controller.selection.take()
            else {
                return Err(Refusal::NotAuthorized);
            };
            let enrollment = broker
                .approve_controller_enrollment(
                    peer.principal,
                    NativeControllerEnrollment::new(proposal),
                    guard,
                )
                .map_err(refusal)?;
            if broker
                .resolve_controller_enrollment(peer.principal, registration, enrollment)
                .ok()
                != Some(target)
            {
                remove_locked(broker, peer, id, guard);
                return Err(Refusal::NotAuthorized);
            }
            let record = peer
                .controller
                .registrations
                .get_mut(&id)
                .ok_or(Refusal::NotAuthorized)?;
            revoke_associated_control(broker, peer.principal, record, guard);
            if let Some(review) = peer
                .controller
                .metadata_review
                .as_ref()
                .filter(|review| review.review.registration() == registration)
            {
                review.valid.set(false);
            }
            record.valid.store(false, Ordering::Release);
            record.valid = Arc::new(AtomicBool::new(true));
            record.association = Some(Association {
                public,
                enrollment,
                target,
                window,
            });
        }
        NativeCommand::UnregisterController {
            channel,
            registration,
        } => {
            let peer = coordinator.peers.get_mut(&channel).ok_or(Refusal::Closed)?;
            if let Some(id) = public_registration(peer, registration) {
                remove_locked(broker, peer, id, guard);
            }
        }
        _ => return Err(Refusal::InvalidArguments),
    }
    Ok(())
}

pub(super) fn cancel_selections(peers: &mut BTreeMap<u64, Peer>) {
    for peer in peers.values_mut() {
        peer.controller.selection = None;
    }
    disarm_metadata_reviews(peers);
}

pub(super) fn disarm_metadata_reviews(peers: &BTreeMap<u64, Peer>) {
    for peer in peers.values() {
        if let Some(review) = &peer.controller.metadata_review {
            review.valid.set(false);
        }
    }
}

pub(super) fn retire_metadata_actions_locked(
    peers: &mut BTreeMap<u64, Peer>,
    pending: &mut BTreeMap<
        festerm_automation::ApprovalId,
        presentation::BoundEnrollmentMetadataReview,
    >,
    grants: &mut BTreeMap<
        presentation::EnrollmentMetadataKey,
        presentation::BoundEnrollmentMetadataGrant,
    >,
    broker: &mut Broker,
    now: Instant,
    guard: &festerm_session::AutomationAdmissionGuard,
) -> festerm_automation::Result<bool> {
    let mut retired = false;
    for peer in peers.values_mut() {
        if peer
            .controller
            .metadata_review
            .as_ref()
            .is_some_and(|review| {
                !review.valid.get()
                    || !pending.contains_key(&review.review.approval_id())
                    || !broker.pending_approval_is_active(
                        review.review.principal(),
                        review.review.approval_id(),
                        now,
                    )
            })
        {
            let review = peer.controller.metadata_review.take().unwrap();
            let principal = review.review.principal();
            let id = review.review.approval_id();
            if principal != peer.principal {
                return Err(festerm_automation::Error::Unauthorized);
            }
            if broker.pending_approval_is_active(principal, id, now) {
                broker.reject_approval(principal, id)?;
            }
            pending.remove(&id);
            retired = true;
        }
        for (registration, enrollment) in std::mem::take(&mut peer.controller.metadata_revocations)
        {
            match broker.resolve_controller_enrollment(peer.principal, registration, enrollment) {
                Ok(_) => broker.revoke_enrollment_metadata_grant(
                    peer.principal,
                    registration,
                    enrollment,
                    guard,
                )?,
                Err(
                    festerm_automation::Error::Unauthorized
                    | festerm_automation::Error::StaleTarget,
                ) => {}
                Err(error) => return Err(error),
            }
            grants.remove(&(peer.principal, registration, enrollment));
            retired = true;
        }
    }
    Ok(retired)
}

pub(super) fn invalidate_location(
    broker: Option<&mut Broker>,
    peers: &mut BTreeMap<u64, Peer>,
    target: Option<TabId>,
    window: Option<WindowId>,
) -> bool {
    if peers.is_empty() {
        return true;
    }
    let Some(broker) = broker else {
        return false;
    };
    let Some(guard) = festerm_session::automation_admission_lock() else {
        return false;
    };
    for peer in peers.values_mut() {
        let stale: Vec<_> = peer
            .controller
            .registrations
            .iter()
            .filter(|(_, record)| {
                record.association.is_some_and(|association| {
                    target.is_some_and(|target| target.chip_id() == association.target.session())
                        || window == Some(association.window)
                })
            })
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            remove_locked(broker, peer, id, &guard);
        }
        if peer.controller.selection.as_ref().is_some_and(|selection| {
            target.is_some()
                || match selection {
                    Selection::Choosing { window: owner, .. } => window == Some(*owner),
                    Selection::Reviewing { candidate, .. } => window == Some(candidate.window),
                }
        }) {
            peer.controller.selection = None;
        }
    }
    true
}

fn local_candidates(
    targets: &BTreeMap<u64, Target>,
    apps: &[&FesTermApp],
) -> (Vec<ControllerTargetRow>, bool) {
    let mut candidates = Vec::new();
    let mut incomplete = false;
    for app in apps {
        for (id, session) in app.automation_sessions() {
            if !is_local_backend(session)
                || !matches!(
                    session.controller.lifecycle(),
                    Some(SessionLifecycle::Running)
                )
            {
                continue;
            }
            if candidates.len() >= MAX_LOCAL_TARGETS {
                incomplete = true;
                continue;
            }
            let Some(target) = targets.get(&id.chip_id()) else {
                incomplete = true;
                continue;
            };
            if !local_target_is_current(target, apps) {
                incomplete = true;
                continue;
            }
            let Some((position, active)) = app.automation_session_context(id) else {
                incomplete = true;
                continue;
            };
            candidates.push(ControllerTargetRow {
                reference: target.reference,
                window: app.automation_window(),
                label: format!(
                    "tab position {}{} — untrusted name \"{}\"",
                    position,
                    if active { ", active" } else { "" },
                    quoted_approval_label(&session.label),
                ),
            });
        }
    }
    if incomplete {
        candidates.clear();
    }
    (candidates, incomplete)
}

pub(super) fn settings(
    coordinator: &Coordinator,
    apps: &[&FesTermApp],
    mcp_open: bool,
) -> (Vec<ControllerRow>, Vec<ControllerTargetRow>, bool) {
    if !mcp_open {
        return (Vec::new(), Vec::new(), false);
    }
    let Ok(guard) = festerm_session::try_automation_admission_lock() else {
        return (Vec::new(), Vec::new(), true);
    };
    let Some(broker) = coordinator.broker.as_ref() else {
        return (Vec::new(), Vec::new(), false);
    };
    let (candidates, incomplete) = local_candidates(&coordinator.targets, apps);
    let rows = coordinator
        .peers
        .iter()
        .filter(|(_, peer)| !peer.connection.is_closed())
        .flat_map(|(channel, peer)| {
            peer.controller
                .registrations
                .iter()
                .filter_map(|(public_id, record)| {
                    if !record.valid.load(Ordering::Acquire) {
                        return None;
                    }
                    let bytes = broker
                        .controller_registration(peer.principal, record.internal)
                        .ok()?;
                    let label = quoted_approval_label(std::str::from_utf8(bytes).ok()?);
                    let associated_target = record.association.and_then(|association| {
                        (broker
                            .resolve_controller_enrollment(
                                peer.principal,
                                record.internal,
                                association.enrollment,
                            )
                            .ok()
                            == Some(association.target)
                            && exact_target(
                                &coordinator.targets,
                                association.target,
                                association.window,
                                apps,
                            )
                            .is_some())
                        .then_some(association.target)
                    });
                    let selection = peer
                        .controller
                        .selection
                        .as_ref()
                        .filter(|selection| selection.registration() == record.internal)
                        .map_or(ControllerSelectionView::Idle, Selection::view);
                    let mut metadata_review_valid = None;
                    let metadata = if coordinator.enrollment_metadata_ui_qualified() {
                        if let Some(association) = record
                            .association
                            .filter(|association| associated_target == Some(association.target))
                        {
                            if peer
                                .controller
                                .metadata_revocations
                                .contains(&(record.internal, association.enrollment))
                            {
                                ControllerMetadataView::Revoking
                            } else if let Some(review) =
                                peer.controller.metadata_review.as_ref().filter(|review| {
                                    review.review.registration() == record.internal
                                        && review.review.enrollment() == association.enrollment
                                        && review.review.target() == association.target
                                        && review.candidate.window == association.window
                                })
                            {
                                if review.valid.get() {
                                    metadata_review_valid = Some(review.valid.clone());
                                    ControllerMetadataView::Reviewing {
                                        token: review.token,
                                        window: review.candidate.window,
                                        target: association.target,
                                    }
                                } else {
                                    ControllerMetadataView::Cancelling
                                }
                            } else {
                                ControllerMetadataView::Idle {
                                    enrollment: association.enrollment,
                                    window: association.window,
                                    target: association.target,
                                    granted: coordinator
                                        .enrollment_metadata_binding_is_current_locked(
                                            *channel,
                                            record.internal,
                                            association.enrollment,
                                            apps,
                                            Instant::now(),
                                            &guard,
                                        ),
                                }
                            }
                        } else {
                            ControllerMetadataView::Unavailable
                        }
                    } else {
                        ControllerMetadataView::Unavailable
                    };
                    let mut input_review_valid = None;
                    let input = if coordinator.direct_input_qualified() {
                        if let Some(pending) =
                            peer.controller.direct_input.as_ref().filter(|pending| {
                                pending.association.registration == record.internal
                                    && associated_target == Some(pending.association.target)
                            })
                        {
                            if !pending.valid.get()
                                || !pending.association.valid.load(Ordering::Acquire)
                                || Instant::now() >= pending.deadline
                            {
                                pending.valid.set(false);
                                ControllerInputView::Cancelling
                            } else {
                                input_review_valid = Some(pending.valid.clone());
                                match &pending.phase {
                                    DirectInputPhase::Requested => ControllerInputView::Requested {
                                        token: pending.token,
                                        window: pending.association.window,
                                        target: pending.association.target,
                                    },
                                    DirectInputPhase::Preparing(capture)
                                    | DirectInputPhase::Reviewing { capture, .. }
                                        if capture_direct_input_review(
                                            coordinator,
                                            *channel,
                                            pending.association.window,
                                            pending,
                                            apps,
                                            &guard,
                                        )
                                        .is_ok_and(|current| current == *capture) =>
                                    {
                                        match &pending.phase {
                                            DirectInputPhase::Preparing(_) => {
                                                ControllerInputView::Preparing {
                                                    token: pending.token,
                                                    window: pending.association.window,
                                                }
                                            }
                                            DirectInputPhase::Reviewing { review, .. } => {
                                                if !coordinator
                                                    .staged_direct_review_is_current_locked(
                                                        *channel, review, apps, &guard,
                                                    )
                                                {
                                                    pending.valid.set(false);
                                                    ControllerInputView::Cancelling
                                                } else if let Some(action) =
                                                    review.direct_action_display()
                                                {
                                                    ControllerInputView::Reviewing {
                                                        token: pending.token,
                                                        window: pending.association.window,
                                                        target: pending.association.target,
                                                        action: action.clone(),
                                                        metadata_sources: capture
                                                            .metadata_descriptions
                                                            .clone(),
                                                    }
                                                } else {
                                                    pending.valid.set(false);
                                                    ControllerInputView::Cancelling
                                                }
                                            }
                                            DirectInputPhase::Requested => unreachable!(),
                                        }
                                    }
                                    _ => {
                                        pending.valid.set(false);
                                        ControllerInputView::Cancelling
                                    }
                                }
                            }
                        } else {
                            ControllerInputView::Unavailable
                        }
                    } else {
                        ControllerInputView::Unavailable
                    };
                    Some(ControllerRow {
                        channel: *channel,
                        registration: record.internal,
                        public_id: *public_id,
                        label,
                        associated_target,
                        selection,
                        metadata,
                        input,
                        valid: record.valid.clone(),
                        metadata_review_valid,
                        input_review_valid,
                    })
                })
        })
        .collect();
    (rows, candidates, incomplete)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn disabled_controller_ui_does_not_wait_for_admission() {
        let guard = festerm_session::automation_admission_lock().unwrap();
        let (started, ready) = mpsc::sync_channel(0);
        let (finished, result) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut coordinator = Coordinator::default();
            started.send(()).unwrap();
            let snapshot = coordinator.settings(&[]);
            coordinator.controller_target_moving(TabId::from_chip_id(1));
            coordinator.controller_window_closing(WindowId::for_test(1));
            finished
                .send(!snapshot.controller_targets_incomplete)
                .unwrap();
        });
        ready.recv().unwrap();
        let completed = result.recv_timeout(Duration::from_secs(5));
        drop(guard);
        worker.join().unwrap();
        assert_eq!(completed, Ok(true));
    }

    fn ready_controller_machine(mut machine: Machine) -> Machine {
        assert!(matches!(
            machine.process_value(json!({
                "jsonrpc": "2.0", "id": 0, "method": "initialize",
                "params": {
                    "protocolVersion": festerm_automation_mcp::PROTOCOL_VERSION,
                    "clientInfo": {"name": "untrusted", "version": "0"},
                    "capabilities": {}
                }
            })),
            Event::Reply(_)
        ));
        assert!(matches!(
            machine.process_value(json!({
                "jsonrpc": "2.0", "method": "notifications/initialized"
            })),
            Event::NoReply
        ));
        machine
    }

    fn unqualified_fixture_with_workspace(
        apps: &[&FesTermApp],
        workspace: &WorkspaceRevision,
    ) -> Coordinator {
        let mut coordinator = Coordinator::default();
        coordinator.bind_presentation_workspace(workspace).unwrap();
        coordinator.broker = Some(Broker::new([2; 16]));
        coordinator.stop = Arc::new(AtomicBool::new(false));
        coordinator.privacy_gate = Arc::new(PrivacyGate::new(false));
        assert!(coordinator.privacy_gate.bind_mcp(&coordinator.stop));
        coordinator.enabled = true;
        let mut broker = coordinator.broker.take().unwrap();
        coordinator.reconcile(&mut broker, apps);
        coordinator.broker = Some(broker);
        coordinator
    }

    fn unqualified_fixture(apps: &[&FesTermApp]) -> Coordinator {
        unqualified_fixture_with_workspace(apps, &WorkspaceRevision::default())
    }

    fn fixture_with_workspace(apps: &[&FesTermApp], workspace: &WorkspaceRevision) -> Coordinator {
        let mut coordinator = unqualified_fixture_with_workspace(apps, workspace);
        coordinator.synthetic_controller_qualified = true;
        native(
            &mut coordinator,
            apps,
            WindowId::PRIMARY,
            NativeCommand::EnableControllerEnrollment,
        );
        assert!(coordinator.controller_opted_in);
        coordinator
    }

    fn fixture(apps: &[&FesTermApp]) -> Coordinator {
        fixture_with_workspace(apps, &WorkspaceRevision::default())
    }

    fn add_peer_with_frames(
        coordinator: &mut Coordinator,
        channel: u64,
    ) -> impl Fn() -> Option<festerm_automation::OutboundFrame> {
        let principal = coordinator
            .broker
            .as_mut()
            .unwrap()
            .accept_connection()
            .unwrap();
        let (connection, receive) = ipc::test_connection_with_frames(channel);
        let machine = ready_controller_machine(coordinator.machine_for_new_peer());
        let controller = PeerState {
            next_test_id: Some((u128::from(channel) << 64) | 1),
            ..PeerState::default()
        };
        coordinator.peers.insert(
            channel,
            Peer {
                connection,
                principal,
                machine,
                code: VerificationCode::new(&format!("{channel:016x}")).unwrap(),
                grant: None,
                waits: Vec::new(),
                pending_completion: None,
                after_finish: None,
                controller,
            },
        );
        receive
    }

    fn add_peer(coordinator: &mut Coordinator, channel: u64) -> impl Fn() -> Option<Vec<u8>> {
        let receive = add_peer_with_frames(coordinator, channel);
        move || receive().map(|frame| frame.bytes)
    }

    fn call(
        coordinator: &mut Coordinator,
        channel: u64,
        apps: &[&FesTermApp],
        name: &str,
        arguments: Value,
    ) {
        call_with_id(coordinator, channel, apps, json!(9), name, arguments);
    }

    fn call_with_id(
        coordinator: &mut Coordinator,
        channel: u64,
        apps: &[&FesTermApp],
        id: Value,
        name: &str,
        arguments: Value,
    ) {
        let bytes = serde_json::to_vec(&json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
        .unwrap();
        let qualified = coordinator.controller_qualified() && coordinator.controller_opted_in;
        let input_qualified = coordinator.direct_input_qualified() && qualified;
        let metadata = EnrollmentMetadataContext {
            grants: &coordinator.enrollment_metadata_grants,
            qualified: coordinator.enrollment_metadata_delivery_qualified() && qualified,
        };
        assert!(coordinator.peers[&channel].pending_completion.is_none());
        let result = process_peer_message(
            coordinator.broker.as_mut().unwrap(),
            &coordinator.targets,
            metadata,
            coordinator.peers.get_mut(&channel).unwrap(),
            &bytes,
            apps,
            &coordinator.privacy_gate,
            &coordinator.stop,
            qualified,
            input_qualified,
            Instant::now(),
        );
        match result {
            Ok(()) => {}
            Err(festerm_session::AutomationAdmissionTryError::Busy) => {
                assert!(coordinator.peers[&channel].pending_completion.is_some());
                retry_automation_busy(|| {
                    retry_completion(
                        coordinator.broker.as_mut().unwrap(),
                        coordinator.peers.get_mut(&channel).unwrap(),
                        Instant::now(),
                    )
                    .map_err(|error| match error {
                        festerm_session::AutomationAdmissionTryError::Busy => {
                            festerm_automation::Error::Busy
                        }
                        festerm_session::AutomationAdmissionTryError::Poisoned => {
                            festerm_automation::Error::Unavailable
                        }
                    })
                })
                .unwrap();
            }
            Err(error) => panic!("peer message admission failed: {error:?}"),
        }
    }

    fn response(receive: &impl Fn() -> Option<Vec<u8>>) -> Value {
        serde_json::from_slice(&receive().expect("expected an outbound response")).unwrap()
    }

    pub(crate) fn stable_settings(
        coordinator: &Coordinator,
        apps: &[&FesTermApp],
    ) -> SettingsSnapshot {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = coordinator.settings(apps);
            if !snapshot.controller_targets_incomplete {
                return snapshot;
            }
            assert!(Instant::now() < deadline, "controller settings stayed busy");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn row(coordinator: &Coordinator, apps: &[&FesTermApp], channel: u64) -> ControllerRow {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = coordinator.settings(apps);
            if let Some(row) = snapshot
                .controllers
                .into_iter()
                .find(|row| row.channel == channel)
            {
                return row;
            }
            assert!(
                snapshot.controller_targets_incomplete && Instant::now() < deadline,
                "registration is visible"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn registration_row(
        coordinator: &Coordinator,
        apps: &[&FesTermApp],
        channel: u64,
        registration: ControllerRegistrationId,
    ) -> ControllerRow {
        stable_settings(coordinator, apps)
            .controllers
            .into_iter()
            .find(|row| row.channel == channel && row.registration == registration)
            .expect("registration is visible")
    }

    fn native(
        coordinator: &mut Coordinator,
        apps: &[&FesTermApp],
        window: WindowId,
        command: NativeCommand,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            coordinator.status = "";
            assert!(!coordinator.commands(
                vec![NativeWindowCommand {
                    window,
                    command: command.clone(),
                }],
                &egui::Context::default(),
                apps,
            ));
            if coordinator.status != BUSY_STATUS {
                return;
            }
            assert!(Instant::now() < deadline, "controller command stayed busy");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn review(
        coordinator: &mut Coordinator,
        apps: &[&FesTermApp],
        window: WindowId,
        channel: u64,
        registration: ControllerRegistrationId,
        target: TargetRef,
    ) -> u64 {
        native(
            coordinator,
            apps,
            window,
            NativeCommand::ChooseController {
                channel,
                registration,
            },
        );
        let ControllerSelectionView::Choosing { token, .. } =
            registration_row(coordinator, apps, channel, registration).selection
        else {
            panic!("native chooser was not opened");
        };
        native(
            coordinator,
            apps,
            window,
            NativeCommand::SelectControllerTarget {
                channel,
                registration,
                selection: token,
                target,
            },
        );
        let ControllerSelectionView::Reviewing {
            token,
            target: selected,
            ..
        } = registration_row(coordinator, apps, channel, registration).selection
        else {
            panic!("native choice was not reviewed");
        };
        assert_eq!(selected, target);
        token
    }

    fn confirm(
        coordinator: &mut Coordinator,
        apps: &[&FesTermApp],
        window: WindowId,
        registration: ControllerRegistrationId,
        token: u64,
    ) {
        native(
            coordinator,
            apps,
            window,
            NativeCommand::ConfirmController {
                channel: 1,
                registration,
                selection: token,
            },
        );
    }

    fn metadata_fixture_with_frames() -> (
        Coordinator,
        FesTermApp,
        TabId,
        ControllerRegistrationId,
        ControllerEnrollmentId,
        impl Fn() -> Option<festerm_automation::OutboundFrame>,
    ) {
        let (app, tab) = FesTermApp::automation_local_fixture();
        let mut coordinator = fixture(&[&app]);
        let receive = add_peer_with_frames(&mut coordinator, 1);
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_controller_register",
            json!({"local_id": "metadata fixture"}),
        );
        receive().expect("registration response");
        let registration = row(&coordinator, &[&app], 1).registration;
        let target = coordinator.targets[&tab.chip_id()].reference;
        let token = review(
            &mut coordinator,
            &[&app],
            WindowId::PRIMARY,
            1,
            registration,
            target,
        );
        confirm(
            &mut coordinator,
            &[&app],
            WindowId::PRIMARY,
            registration,
            token,
        );
        let enrollment = coordinator.peers[&1]
            .controller
            .registrations
            .values()
            .find(|record| record.internal == registration)
            .unwrap()
            .association
            .unwrap()
            .enrollment;
        (coordinator, app, tab, registration, enrollment, receive)
    }

    fn metadata_fixture() -> (
        Coordinator,
        FesTermApp,
        TabId,
        ControllerRegistrationId,
        ControllerEnrollmentId,
    ) {
        let (coordinator, app, tab, registration, enrollment, _receive) =
            metadata_fixture_with_frames();
        (coordinator, app, tab, registration, enrollment)
    }

    fn metadata_command_with_retry(
        coordinator: &mut Coordinator,
        app: &FesTermApp,
        window: WindowId,
        action: NativeCommand,
    ) -> Result<(), CommandError> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match command(coordinator, window, action.clone(), &[app]) {
                Err(CommandError::Busy) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                result => return result,
            }
        }
    }

    fn metadata_review_token(
        coordinator: &Coordinator,
        app: &FesTermApp,
        registration: ControllerRegistrationId,
    ) -> u64 {
        match registration_row(coordinator, &[app], 1, registration).metadata {
            ControllerMetadataView::Reviewing { token, .. } => token,
            _ => panic!("expected metadata review"),
        }
    }

    fn approve_metadata_for_delivery(
        coordinator: &mut Coordinator,
        app: &FesTermApp,
        channel: u64,
        registration: ControllerRegistrationId,
        enrollment: ControllerEnrollmentId,
        lifetime: Duration,
    ) {
        let review = retry_automation_busy(|| {
            coordinator.prepare_enrollment_metadata_review(
                channel,
                registration,
                enrollment,
                lifetime,
                &[app],
            )
        })
        .unwrap();
        let decision = review.accept_from_native();
        retry_automation_busy(|| {
            coordinator.approve_enrollment_metadata_review(channel, &review, &decision, &[app])
        })
        .unwrap();
    }

    fn metadata_handle(
        coordinator: &Coordinator,
        registration: ControllerRegistrationId,
    ) -> String {
        coordinator.peers[&1]
            .controller
            .registrations
            .values()
            .find(|record| record.internal == registration)
            .unwrap()
            .association
            .unwrap()
            .public
            .to_wire()
    }

    fn frame_response(frame: &festerm_automation::OutboundFrame) -> Value {
        serde_json::from_slice(&frame.bytes).unwrap()
    }

    fn queue_direct_input_with_retry(
        coordinator: &mut Coordinator,
        app: &FesTermApp,
        arguments: &Value,
        receive: &impl Fn() -> Option<festerm_automation::OutboundFrame>,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            call_with_id(
                coordinator,
                1,
                &[app],
                json!(9),
                "festerm_terminal_send_text",
                arguments.clone(),
            );
            if coordinator.peers[&1].controller.direct_input.is_some() {
                assert!(receive().is_none());
                return;
            }
            assert_eq!(
                frame_response(&receive().expect("expected transient refusal"))["result"]
                    ["structuredContent"]["code"],
                "UNAVAILABLE"
            );
            assert!(Instant::now() < deadline, "input admission stayed busy");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn reviewing_input_fixture(
        text: &str,
        metadata_lifetime: Option<Duration>,
        separate_metadata_lifetime: Option<Duration>,
    ) -> (
        Coordinator,
        FesTermApp,
        TabId,
        ControllerRegistrationId,
        ControllerEnrollmentId,
        u64,
        impl Fn() -> Option<festerm_automation::OutboundFrame>,
    ) {
        let (mut coordinator, mut app, tab, registration, enrollment, receive) =
            metadata_fixture_with_frames();
        let ime = Arc::new(festerm_session::AutomationEpoch::new());
        ime.transition(true);
        app.set_automation_ime_epoch(&ime);
        coordinator.synthetic_direct_input_qualified = true;
        coordinator.peers.get_mut(&1).unwrap().machine =
            ready_controller_machine(Machine::with_direct_input_catalog());
        if let Some(lifetime) = separate_metadata_lifetime {
            let target = coordinator.targets[&tab.chip_id()].reference;
            let review = retry_automation_busy(|| {
                coordinator.prepare_metadata_review(
                    1,
                    festerm_automation::MetadataGrantRequest {
                        targets: BTreeSet::from([target]),
                    },
                    lifetime,
                )
            })
            .unwrap();
            let decision = review.accept_from_native();
            retry_automation_busy(|| coordinator.approve_metadata_review(1, &review, &decision))
                .unwrap();
        }
        if let Some(lifetime) = metadata_lifetime {
            approve_metadata_for_delivery(
                &mut coordinator,
                &app,
                1,
                registration,
                enrollment,
                lifetime,
            );
        }
        let enrollment_ref = metadata_handle(&coordinator, registration);
        queue_direct_input_with_retry(
            &mut coordinator,
            &app,
            &json!({
                "enrollment_ref": enrollment_ref,
                "text": text,
                "submit": true,
            }),
            &receive,
        );
        let row = registration_row(&coordinator, &[&app], 1, registration);
        let ControllerInputView::Requested { token, .. } = row.input else {
            panic!("expected input request");
        };
        native(
            &mut coordinator,
            &[&app],
            WindowId::PRIMARY,
            NativeCommand::ReviewControllerInput {
                channel: 1,
                registration,
                input: token,
            },
        );
        let preparations = direct_input_preparations(&coordinator);
        assert_eq!(preparations.len(), 1);
        retry_automation_busy(|| {
            prepare_direct_input_review(
                &mut coordinator,
                preparations[0],
                app.automation_session_mut(tab),
            )
        })
        .unwrap();
        (
            coordinator,
            app,
            tab,
            registration,
            enrollment,
            token,
            receive,
        )
    }

    #[test]
    fn direct_input_wire_queue_is_bounded_cancellable_and_expires() {
        let (mut coordinator, app, _tab, registration, _enrollment, receive) =
            metadata_fixture_with_frames();
        coordinator.synthetic_direct_input_qualified = true;
        coordinator.peers.get_mut(&1).unwrap().machine =
            ready_controller_machine(Machine::with_direct_input_catalog());
        let handle = metadata_handle(&coordinator, registration);
        let arguments = json!({"enrollment_ref": handle, "text": "private input"});
        queue_direct_input_with_retry(&mut coordinator, &app, &arguments, &receive);
        let pending = coordinator.peers[&1]
            .controller
            .direct_input
            .as_ref()
            .unwrap();
        let original_key = pending.request.key.clone();
        let valid = pending.valid.clone();
        call_with_id(
            &mut coordinator,
            1,
            &[&app],
            json!(10),
            "festerm_terminal_send_text",
            arguments.clone(),
        );
        assert_eq!(
            frame_response(&receive().unwrap())["result"]["structuredContent"],
            json!({"status": "refused", "code": "BUDGET_EXCEEDED"})
        );
        let cancelled =
            br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":9}}"#;
        process_peer_message(
            coordinator.broker.as_mut().unwrap(),
            &coordinator.targets,
            EnrollmentMetadataContext {
                grants: &coordinator.enrollment_metadata_grants,
                qualified: false,
            },
            coordinator.peers.get_mut(&1).unwrap(),
            cancelled,
            &[&app],
            &coordinator.privacy_gate,
            &coordinator.stop,
            true,
            true,
            Instant::now(),
        )
        .unwrap();
        assert!(!valid.get());
        assert!(coordinator.peers[&1].controller.direct_input.is_none());
        assert!(receive().is_none());
        queue_direct_input_with_retry(&mut coordinator, &app, &arguments, &receive);
        let pending = coordinator.peers[&1]
            .controller
            .direct_input
            .as_ref()
            .unwrap();
        assert_ne!(pending.request.key, original_key);
        let deadline = pending.deadline;
        let valid = pending.valid.clone();
        expire_direct_input(coordinator.peers.get_mut(&1).unwrap(), deadline);
        assert!(!valid.get());
        assert_eq!(
            frame_response(&receive().unwrap())["result"]["structuredContent"],
            json!({"status": "refused", "code": "UNAVAILABLE"})
        );
        assert!(coordinator.peers[&1].controller.direct_input.is_none());
    }

    #[test]
    fn direct_input_requires_its_connection_and_invalidates_on_target_move() {
        let (mut coordinator, app, tab, registration, _enrollment, receive) =
            metadata_fixture_with_frames();
        coordinator.synthetic_direct_input_qualified = true;
        coordinator.peers.get_mut(&1).unwrap().machine =
            ready_controller_machine(Machine::with_direct_input_catalog());
        let other_receive = add_peer_with_frames(&mut coordinator, 2);
        let arguments = json!({
            "enrollment_ref": metadata_handle(&coordinator, registration),
            "text": "input"
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            call_with_id(
                &mut coordinator,
                2,
                &[&app],
                json!(10),
                "festerm_terminal_send_text",
                arguments.clone(),
            );
            let result = frame_response(&other_receive().expect("expected refusal"));
            match result["result"]["structuredContent"]["code"].as_str() {
                Some("NOT_AUTHORIZED") => break,
                Some("UNAVAILABLE") if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                _ => panic!("unexpected direct-input refusal"),
            }
        }
        assert!(coordinator.peers[&2].controller.direct_input.is_none());
        let mut presented = arguments.clone();
        presented["presentation_revision"] = json!("a".repeat(48));
        call_with_id(
            &mut coordinator,
            1,
            &[&app],
            json!(11),
            "festerm_terminal_send_text",
            presented,
        );
        assert_eq!(
            frame_response(&receive().unwrap())["result"]["structuredContent"]["code"],
            "UNAVAILABLE"
        );
        queue_direct_input_with_retry(&mut coordinator, &app, &arguments, &receive);
        let pending = coordinator.peers[&1]
            .controller
            .direct_input
            .as_ref()
            .unwrap();
        let valid = pending.valid.clone();
        coordinator.controller_target_moving(tab);
        assert!(!valid.get());
        expire_direct_input(coordinator.peers.get_mut(&1).unwrap(), Instant::now());
        assert_eq!(
            frame_response(&receive().unwrap())["result"]["structuredContent"]["code"],
            "NOT_AUTHORIZED"
        );
        assert!(coordinator.peers[&1].controller.direct_input.is_none());
    }

    #[test]
    fn direct_input_native_review_captures_metadata_and_cancellation_invalidates_it() {
        for expire_source in [false, true] {
            let text = format!("{}\n\"\\\u{202e}end", "x".repeat(140));
            let (mut coordinator, app, _tab, registration, _enrollment, token, receive) =
                reviewing_input_fixture(&text, Some(Duration::from_secs(60)), None);
            let snapshot = stable_settings(&coordinator, &[&app]);
            let row = snapshot
                .controllers
                .iter()
                .find(|row| row.registration == registration)
                .unwrap();
            let ControllerInputView::Reviewing {
                action,
                metadata_sources,
                ..
            } = &row.input
            else {
                panic!("expected frozen input review");
            };
            assert_eq!(
                action,
                &presentation::DirectActionDisplay::Text {
                    escaped: format!("{}\\n\\\"\\\\\\u{{202e}}end", "x".repeat(140)),
                    submit: true,
                }
            );
            assert_eq!(metadata_sources.len(), 1);
            assert!(metadata_sources[0].contains("metadata fixture"));
            assert!(metadata_sources[0].contains(&metadata_handle(&coordinator, registration)));
            assert!(snapshot.controller_input_review_is_current(row));
            if expire_source {
                let pending = coordinator.peers[&1]
                    .controller
                    .direct_input
                    .as_ref()
                    .unwrap();
                let deadline = pending.next_deadline();
                assert!(deadline < pending.deadline);
                expire_direct_input(coordinator.peers.get_mut(&1).unwrap(), deadline);
                assert!(!snapshot.controller_input_review_is_current(row));
                let mut broker = coordinator.broker.take().unwrap();
                coordinator.reconcile(&mut broker, &[&app]);
                coordinator.broker = Some(broker);
                assert!(coordinator.pending_control_reviews.is_empty());
                assert_eq!(
                    frame_response(&receive().unwrap())["result"]["structuredContent"]["code"],
                    "NOT_AUTHORIZED"
                );
                continue;
            }
            let phase = std::mem::replace(
                &mut coordinator
                    .peers
                    .get_mut(&1)
                    .unwrap()
                    .controller
                    .direct_input
                    .as_mut()
                    .unwrap()
                    .phase,
                DirectInputPhase::Requested,
            );
            let DirectInputPhase::Reviewing { review, .. } = phase else {
                panic!("expected pending host review");
            };
            let decision = review.accept_from_native();
            snapshot.cancel_controller_input_review(1, registration, token);
            assert!(!snapshot.controller_input_review_is_current(row));
            assert_eq!(
                retry_automation_busy(|| coordinator.approve_control_handoff(1, &review, &decision)),
                Err(festerm_automation::Error::Revoked)
            );
            let mut broker = coordinator.broker.take().unwrap();
            coordinator.reconcile(&mut broker, &[&app]);
            coordinator.broker = Some(broker);
            assert!(coordinator.pending_control_reviews.is_empty());
            expire_direct_input(coordinator.peers.get_mut(&1).unwrap(), Instant::now());
            assert_eq!(
                frame_response(&receive().unwrap())["result"]["structuredContent"]["code"],
                "NOT_AUTHORIZED"
            );
        }
    }

    #[test]
    fn direct_input_wire_cancellation_reclaims_review_in_the_same_tick() {
        let (mut coordinator, mut app, _, _, _, _, receive) =
            reviewing_input_fixture("cancelled", None, None);
        let principal = coordinator.peers[&1].principal;
        let approval = *coordinator.pending_control_reviews.keys().next().unwrap();
        let settings = festerm_config::InterfaceSettings::DEFAULT.with_mcp_enabled(true);
        app.adopt_broadcast_configuration(
            festerm_config::Configuration::empty()
                .with_interface_settings(settings)
                .unwrap(),
        );
        let (server, events) = ipc::test_server(coordinator.stop.clone());
        coordinator.server = Some(server);
        assert!(events
            .try_send(ipc::Event::Message {
                key: 1,
                bytes: br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":9}}"#
                    .to_vec(),
            })
            .is_ok());
        let context = egui::Context::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            coordinator.tick(&context, &[&app]);
            if coordinator.peers[&1].controller.direct_input.is_none()
                && !coordinator.reconciliation_deferred
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "cancellation cleanup stayed busy"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(coordinator.pending_control_reviews.is_empty());
        assert!(!coordinator
            .broker
            .as_ref()
            .unwrap()
            .pending_approval_is_active(principal, approval, Instant::now(),));
        assert!(receive().is_none());
    }

    #[test]
    fn direct_input_review_expires_with_earliest_separate_metadata_grant() {
        let (mut coordinator, app, _, registration, _, _, receive) = reviewing_input_fixture(
            "metadata expires",
            Some(Duration::from_secs(60)),
            Some(Duration::from_secs(30)),
        );
        let snapshot = stable_settings(&coordinator, &[&app]);
        let row = snapshot
            .controllers
            .iter()
            .find(|row| row.registration == registration)
            .unwrap();
        let ControllerInputView::Reviewing {
            metadata_sources, ..
        } = &row.input
        else {
            panic!("expected frozen input review");
        };
        assert_eq!(metadata_sources.len(), 2);
        assert!(metadata_sources
            .iter()
            .any(|source| source.starts_with("Separate metadata grant:")));
        assert!(metadata_sources
            .iter()
            .any(|source| source.contains(&metadata_handle(&coordinator, registration))));
        let pending = coordinator.peers[&1]
            .controller
            .direct_input
            .as_ref()
            .unwrap();
        let deadline = pending.next_deadline();
        let enrollment_expiry = coordinator
            .enrollment_metadata_grants
            .values()
            .next()
            .unwrap()
            .grant
            .expires();
        assert!(deadline < pending.deadline && deadline < enrollment_expiry);
        expire_direct_input(coordinator.peers.get_mut(&1).unwrap(), deadline);
        assert!(!snapshot.controller_input_review_is_current(row));
        let mut broker = coordinator.broker.take().unwrap();
        coordinator.reconcile(&mut broker, &[&app]);
        coordinator.broker = Some(broker);
        assert!(coordinator.pending_control_reviews.is_empty());
        assert_eq!(
            frame_response(&receive().unwrap())["result"]["structuredContent"]["code"],
            "NOT_AUTHORIZED"
        );
    }

    #[test]
    fn direct_input_review_retires_changed_human_fences_modes_and_broker_decisions() {
        for change in ["human", "mode", "decision"] {
            let (mut coordinator, mut app, tab, registration, _, _, _receive) =
                reviewing_input_fixture("mode-sensitive", None, None);
            let snapshot = stable_settings(&coordinator, &[&app]);
            let old = snapshot
                .controllers
                .iter()
                .find(|row| row.registration == registration)
                .unwrap();
            assert!(matches!(old.input, ControllerInputView::Reviewing { .. }));
            let human = if change == "human" {
                Some(
                    app.automation_session_mut(tab)
                        .controller
                        .begin_pending_human_input(),
                )
            } else {
                None
            };
            if change == "mode" {
                app.automation_session_mut(tab)
                    .terminal
                    .ingest(b"\x1b[?2004h");
            } else if change == "decision" {
                let principal = coordinator.peers[&1].principal;
                let approval = *coordinator.pending_control_reviews.keys().next().unwrap();
                coordinator
                    .broker
                    .as_mut()
                    .unwrap()
                    .reject_approval(principal, approval)
                    .unwrap();
            }
            let current = registration_row(&coordinator, &[&app], 1, registration);
            assert!(matches!(current.input, ControllerInputView::Cancelling));
            assert!(!snapshot.controller_input_review_is_current(old));
            drop(human);
            let mut broker = coordinator.broker.take().unwrap();
            coordinator.reconcile(&mut broker, &[&app]);
            coordinator.broker = Some(broker);
            assert!(coordinator.pending_control_reviews.is_empty());
        }
    }

    #[test]
    fn direct_input_review_displays_all_enrollments_and_retires_stale_sources() {
        let (mut coordinator, mut app, first_tab, first, first_enrollment, receive) =
            metadata_fixture_with_frames();
        let (mut donor, second_tab) = FesTermApp::automation_local_fixture();
        app.adopt_tab(donor.detach_tab(second_tab).unwrap(), None);
        app.automation_session_mut(second_tab).label = "other\nsource".to_owned();
        let mut broker = coordinator.broker.take().unwrap();
        coordinator.reconcile(&mut broker, &[&app]);
        coordinator.broker = Some(broker);
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_controller_register",
            json!({"local_id": "second source"}),
        );
        receive().expect("second registration response");
        let second = coordinator.peers[&1]
            .controller
            .registrations
            .values()
            .map(|record| record.internal)
            .find(|registration| *registration != first)
            .unwrap();
        let second_target = coordinator.targets[&second_tab.chip_id()].reference;
        let selection = review(
            &mut coordinator,
            &[&app],
            WindowId::PRIMARY,
            1,
            second,
            second_target,
        );
        confirm(
            &mut coordinator,
            &[&app],
            WindowId::PRIMARY,
            second,
            selection,
        );
        let second_enrollment = coordinator.peers[&1]
            .controller
            .registrations
            .values()
            .find(|record| record.internal == second)
            .unwrap()
            .association
            .unwrap()
            .enrollment;
        for (registration, enrollment) in [(first, first_enrollment), (second, second_enrollment)] {
            approve_metadata_for_delivery(
                &mut coordinator,
                &app,
                1,
                registration,
                enrollment,
                Duration::from_secs(60),
            );
        }
        let ime = Arc::new(festerm_session::AutomationEpoch::new());
        ime.transition(true);
        app.set_automation_ime_epoch(&ime);
        coordinator.synthetic_direct_input_qualified = true;
        coordinator.peers.get_mut(&1).unwrap().machine =
            ready_controller_machine(Machine::with_direct_input_catalog());
        let first_handle = metadata_handle(&coordinator, first);
        let second_handle = metadata_handle(&coordinator, second);
        queue_direct_input_with_retry(
            &mut coordinator,
            &app,
            &json!({"enrollment_ref": &first_handle, "text": "safe", "submit": false}),
            &receive,
        );
        let row = registration_row(&coordinator, &[&app], 1, first);
        let ControllerInputView::Requested { token, .. } = row.input else {
            panic!("expected input request");
        };
        native(
            &mut coordinator,
            &[&app],
            WindowId::PRIMARY,
            NativeCommand::ReviewControllerInput {
                channel: 1,
                registration: first,
                input: token,
            },
        );
        let preparation = direct_input_preparations(&coordinator)[0];
        retry_automation_busy(|| {
            prepare_direct_input_review(
                &mut coordinator,
                preparation,
                app.automation_session_mut(first_tab),
            )
        })
        .unwrap();
        let snapshot = stable_settings(&coordinator, &[&app]);
        let row = snapshot
            .controllers
            .iter()
            .find(|row| row.registration == first)
            .unwrap();
        let ControllerInputView::Reviewing {
            metadata_sources, ..
        } = &row.input
        else {
            panic!("expected frozen input review");
        };
        assert_eq!(metadata_sources.len(), 2);
        assert!(metadata_sources
            .iter()
            .any(|source| source.contains(&first_handle)));
        assert!(metadata_sources.iter().any(|source| {
            source.contains(&second_handle)
                && source.contains("second source")
                && source.contains("other\\nsource")
        }));

        coordinator.controller_target_moving(second_tab);
        let current = registration_row(&coordinator, &[&app], 1, first);
        assert!(matches!(current.input, ControllerInputView::Cancelling));
        assert!(!snapshot.controller_input_review_is_current(row));
        let mut broker = coordinator.broker.take().unwrap();
        coordinator.reconcile(&mut broker, &[&app]);
        coordinator.broker = Some(broker);
        assert!(coordinator.pending_control_reviews.is_empty());
    }

    #[test]
    fn direct_input_handoff_is_revoked_only_when_its_enrollment_is_retired() {
        for change in ["unregister", "replace", "unrelated", "replacement-grant"] {
            let (mut coordinator, mut app, tab, registration, _, _, receive) =
                reviewing_input_fixture("handoff", None, None);
            let principal = coordinator.peers[&1].principal;
            let target = coordinator.targets[&tab.chip_id()].reference;
            let phase = std::mem::replace(
                &mut coordinator
                    .peers
                    .get_mut(&1)
                    .unwrap()
                    .controller
                    .direct_input
                    .as_mut()
                    .unwrap()
                    .phase,
                DirectInputPhase::Requested,
            );
            let DirectInputPhase::Reviewing {
                review: pending_review,
                ..
            } = phase
            else {
                panic!("expected pending host review");
            };
            let decision = pending_review.accept_from_native();
            retry_automation_busy(|| {
                coordinator.approve_control_handoff(1, &pending_review, &decision)
            })
            .unwrap();
            let grant = coordinator
                .broker
                .as_ref()
                .unwrap()
                .native_control_grant(principal, target, Instant::now())
                .unwrap();
            let mut expected = grant;
            match change {
                "unregister" => native(
                    &mut coordinator,
                    &[&app],
                    WindowId::PRIMARY,
                    NativeCommand::UnregisterController {
                        channel: 1,
                        registration,
                    },
                ),
                "replace" => {
                    let selection = review(
                        &mut coordinator,
                        &[&app],
                        WindowId::PRIMARY,
                        1,
                        registration,
                        target,
                    );
                    confirm(
                        &mut coordinator,
                        &[&app],
                        WindowId::PRIMARY,
                        registration,
                        selection,
                    );
                }
                "unrelated" => {
                    call_with_id(
                        &mut coordinator,
                        1,
                        &[&app],
                        json!(12),
                        "festerm_controller_register",
                        json!({"local_id": "unrelated"}),
                    );
                    receive().expect("unrelated registration response");
                    let other = coordinator.peers[&1]
                        .controller
                        .registrations
                        .values()
                        .map(|record| record.internal)
                        .find(|other| *other != registration)
                        .unwrap();
                    native(
                        &mut coordinator,
                        &[&app],
                        WindowId::PRIMARY,
                        NativeCommand::UnregisterController {
                            channel: 1,
                            registration: other,
                        },
                    );
                }
                "replacement-grant" => {
                    coordinator
                        .peers
                        .get_mut(&1)
                        .unwrap()
                        .controller
                        .direct_input = None;
                    let successor = retry_automation_busy(|| {
                        coordinator.prepare_direct_action_review(
                            1,
                            target,
                            presentation::DirectAction::Text {
                                text: "different",
                                submit: false,
                            },
                            Duration::from_secs(30),
                            app.automation_session_mut(tab),
                        )
                    })
                    .unwrap();
                    let decision = successor.accept_from_native();
                    retry_automation_busy(|| {
                        coordinator.approve_control_handoff(1, &successor, &decision)
                    })
                    .unwrap();
                    expected = coordinator
                        .broker
                        .as_ref()
                        .unwrap()
                        .native_control_grant(principal, target, Instant::now())
                        .unwrap();
                    assert_ne!(expected, grant);
                    native(
                        &mut coordinator,
                        &[&app],
                        WindowId::PRIMARY,
                        NativeCommand::UnregisterController {
                            channel: 1,
                            registration,
                        },
                    );
                }
                _ => unreachable!(),
            }
            let current = coordinator.broker.as_ref().unwrap().native_control_grant(
                principal,
                target,
                Instant::now(),
            );
            if matches!(change, "unrelated" | "replacement-grant") {
                assert_eq!(current, Ok(expected));
            } else {
                assert!(current.is_err(), "{change} must revoke the handoff");
            }
        }
    }

    #[test]
    fn direct_input_cancellation_after_approval_cuts_the_original_handoff() {
        for change in ["wire", "native", "expiry"] {
            let (mut coordinator, app, tab, registration, _, token, _receive) =
                reviewing_input_fixture("cancel handoff", None, None);
            let principal = coordinator.peers[&1].principal;
            let target = coordinator.targets[&tab.chip_id()].reference;
            let phase = std::mem::replace(
                &mut coordinator
                    .peers
                    .get_mut(&1)
                    .unwrap()
                    .controller
                    .direct_input
                    .as_mut()
                    .unwrap()
                    .phase,
                DirectInputPhase::Requested,
            );
            let DirectInputPhase::Reviewing { review, .. } = phase else {
                panic!("expected pending host review");
            };
            let decision = review.accept_from_native();
            retry_automation_busy(|| coordinator.approve_control_handoff(1, &review, &decision))
                .unwrap();
            let spec = RunSpec {
                binding: *review.request().bindings.iter().next().unwrap(),
                scope: review.request().scope.clone(),
                budget: Budget::default(),
                deadline: Instant::now() + Duration::from_secs(30),
            };
            let run = coordinator
                .broker
                .as_mut()
                .unwrap()
                .admit_run(principal, None, spec, Instant::now())
                .unwrap();
            match change {
                "wire" => {
                    let metadata = EnrollmentMetadataContext {
                        grants: &coordinator.enrollment_metadata_grants,
                        qualified: false,
                    };
                    process_peer_message(
                        coordinator.broker.as_mut().unwrap(),
                        &coordinator.targets,
                        metadata,
                        coordinator.peers.get_mut(&1).unwrap(),
                        br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":9}}"#,
                        &[&app],
                        &coordinator.privacy_gate,
                        &coordinator.stop,
                        true,
                        true,
                        Instant::now(),
                    )
                    .unwrap();
                }
                "native" => native(
                    &mut coordinator,
                    &[&app],
                    WindowId::PRIMARY,
                    NativeCommand::CancelControllerInput {
                        channel: 1,
                        registration,
                        input: token,
                    },
                ),
                "expiry" => {
                    let deadline = coordinator.peers[&1]
                        .controller
                        .direct_input
                        .as_ref()
                        .unwrap()
                        .deadline;
                    expire_direct_input(coordinator.peers.get_mut(&1).unwrap(), deadline);
                }
                _ => unreachable!(),
            }
            assert!(presentation::has_cancelled_direct_handoffs(
                &coordinator.control_handoffs,
                Instant::now(),
            ));
            assert!(retry_automation_busy(|| {
                coordinator.acquire_control_lease(1, run, target)
            })
            .is_err());
            assert!(coordinator
                .broker
                .as_ref()
                .unwrap()
                .native_control_grant(principal, target, Instant::now())
                .is_err());
            assert!(coordinator.control_handoffs.is_empty());
        }
    }

    #[test]
    fn enrollment_metadata_route_requires_separate_approval_and_original_liveness() {
        let (mut coordinator, app, tab, registration, enrollment, receive) =
            metadata_fixture_with_frames();
        coordinator.synthetic_enrollment_metadata_ui_qualified = true;
        coordinator.synthetic_enrollment_metadata_delivery_qualified = true;
        assert!(!app.automation_javascript_enabled());
        assert!(!coordinator.javascript_enabled);
        assert!(coordinator.peers[&1].grant.is_none());
        let handle = metadata_handle(&coordinator, registration);

        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_controller_capabilities",
            json!({}),
        );
        let capabilities = frame_response(&receive().unwrap());
        assert_eq!(
            capabilities["result"]["structuredContent"]["data"]["metadata_available"],
            true
        );
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_sessions_get",
            json!({"session_ref": handle}),
        );
        assert_eq!(
            frame_response(&receive().unwrap())["result"]["structuredContent"]["code"],
            "NOT_AUTHORIZED"
        );

        metadata_command_with_retry(
            &mut coordinator,
            &app,
            WindowId::PRIMARY,
            NativeCommand::ReviewControllerMetadata {
                channel: 1,
                registration,
                enrollment,
            },
        )
        .unwrap();
        let token = metadata_review_token(&coordinator, &app, registration);
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_sessions_get",
            json!({"session_ref": handle}),
        );
        assert_eq!(
            frame_response(&receive().unwrap())["result"]["structuredContent"]["code"],
            "NOT_AUTHORIZED"
        );
        metadata_command_with_retry(
            &mut coordinator,
            &app,
            WindowId::PRIMARY,
            NativeCommand::ConfirmControllerMetadata {
                channel: 1,
                registration,
                review: token,
            },
        )
        .unwrap();
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_sessions_get",
            json!({"session_ref": handle}),
        );
        let frame = receive().expect("metadata response");
        assert!(frame.is_valid());
        let mut delivered = Vec::new();
        ipc::write_test_frame(&mut delivered, &frame).unwrap();
        assert_eq!(&delivered[4..], frame.bytes.as_slice());
        let metadata_response = frame_response(&frame);
        assert_eq!(
            metadata_response["result"]["structuredContent"]["data"],
            json!({
                "sessions": [{
                    "session_ref": handle,
                    "label": app.automation_session(tab).unwrap().label.as_str(),
                    "lifecycle": "running",
                    "untrusted": true,
                }],
                "untrusted": true,
            })
        );
        let raw_target =
            serde_json::to_string(&coordinator.targets[&tab.chip_id()].reference).unwrap();
        assert!(!String::from_utf8_lossy(&frame.bytes).contains(&raw_target));
        assert!(coordinator.peers[&1].pending_completion.is_none());

        let other_receive = add_peer(&mut coordinator, 2);
        call(
            &mut coordinator,
            2,
            &[&app],
            "festerm_sessions_get",
            json!({"session_ref": handle}),
        );
        assert_eq!(
            response(&other_receive)["result"]["structuredContent"]["code"],
            "NOT_AUTHORIZED"
        );
        let (principal, liveness) = frame.disclosure.as_deref().unwrap();
        assert!(liveness.is_live(*principal, Instant::now()));
        assert!(!liveness.is_live(coordinator.peers[&2].principal, Instant::now()));

        retry_automation_busy(|| {
            coordinator.revoke_enrollment_metadata_grant(1, registration, enrollment)
        })
        .unwrap();
        assert!(!frame.is_valid());
        let mut refused = Vec::new();
        assert_eq!(
            ipc::write_test_frame(&mut refused, &frame)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::Interrupted
        );
        assert!(refused.is_empty());
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_sessions_get",
            json!({"session_ref": handle}),
        );
        assert_eq!(
            frame_response(&receive().unwrap())["result"]["structuredContent"]["code"],
            "NOT_AUTHORIZED"
        );
    }

    #[test]
    fn enrollment_metadata_and_direct_input_require_reviewed_cross_disclosure() {
        use festerm_automation::Error;
        use presentation::DirectAction;

        let (mut coordinator, mut app, tab, registration, enrollment, receive) =
            metadata_fixture_with_frames();
        coordinator.synthetic_enrollment_metadata_ui_qualified = true;
        coordinator.synthetic_enrollment_metadata_delivery_qualified = true;
        let ime = Arc::new(festerm_session::AutomationEpoch::new());
        ime.transition(true);
        app.set_automation_ime_epoch(&ime);
        approve_metadata_for_delivery(
            &mut coordinator,
            &app,
            1,
            registration,
            enrollment,
            Duration::from_secs(60),
        );
        let principal = coordinator.peers[&1].principal;
        let target = coordinator.targets[&tab.chip_id()].reference;
        let metadata = coordinator
            .broker
            .as_ref()
            .unwrap()
            .native_enrollment_metadata_grant(principal, registration, enrollment, Instant::now())
            .unwrap();

        assert!(matches!(
            retry_automation_busy(|| coordinator.prepare_direct_action_review(
                1,
                target,
                DirectAction::Text {
                    text: "hello",
                    submit: false,
                },
                Duration::from_secs(60),
                app.automation_session_mut(tab),
            )),
            Err(Error::Unauthorized)
        ));
        assert!(coordinator.pending_control_reviews.is_empty());

        let sources = Scope {
            discover: BTreeSet::from([target]),
            ..Scope::default()
        };
        let review = retry_automation_busy(|| {
            coordinator.prepare_direct_action_review_with_sources(
                1,
                target,
                DirectAction::Text {
                    text: "hello",
                    submit: false,
                },
                Duration::from_secs(60),
                app.automation_session_mut(tab),
                &sources,
            )
        })
        .unwrap();
        assert_eq!(review.metadata_sources(), &BTreeSet::from([target]));
        assert_eq!(review.enrollment_metadata_grants(), &[metadata]);
        let decision = review.accept_from_native();
        retry_automation_busy(|| coordinator.approve_control_handoff(1, &review, &decision))
            .unwrap();
        assert!(coordinator.peers[&1].grant.is_none());
        assert!(!coordinator.javascript_enabled);

        let handle = metadata_handle(&coordinator, registration);
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_sessions_get",
            json!({"session_ref": handle}),
        );
        let response = frame_response(&receive().unwrap());
        assert_eq!(response["result"]["structuredContent"]["status"], "ok");
        assert_eq!(
            coordinator
                .broker
                .as_ref()
                .unwrap()
                .native_enrollment_metadata_grant(
                    principal,
                    registration,
                    enrollment,
                    Instant::now(),
                ),
            Ok(metadata)
        );
    }

    #[test]
    fn direct_input_cross_disclosure_review_does_not_follow_metadata_replacement() {
        use festerm_automation::Error;
        use presentation::DirectAction;

        let (mut coordinator, mut app, tab, registration, enrollment) = metadata_fixture();
        coordinator.synthetic_enrollment_metadata_ui_qualified = true;
        let ime = Arc::new(festerm_session::AutomationEpoch::new());
        ime.transition(true);
        app.set_automation_ime_epoch(&ime);
        approve_metadata_for_delivery(
            &mut coordinator,
            &app,
            1,
            registration,
            enrollment,
            Duration::from_secs(60),
        );
        let target = coordinator.targets[&tab.chip_id()].reference;
        let sources = Scope {
            discover: BTreeSet::from([target]),
            ..Scope::default()
        };
        let review = retry_automation_busy(|| {
            coordinator.prepare_direct_action_review_with_sources(
                1,
                target,
                DirectAction::Keys(&["ctrl:c".to_owned()]),
                Duration::from_secs(60),
                app.automation_session_mut(tab),
                &sources,
            )
        })
        .unwrap();
        let decision = review.accept_from_native();
        retry_automation_busy(|| {
            coordinator.revoke_enrollment_metadata_grant(1, registration, enrollment)
        })
        .unwrap();
        approve_metadata_for_delivery(
            &mut coordinator,
            &app,
            1,
            registration,
            enrollment,
            Duration::from_secs(60),
        );
        assert!(matches!(
            retry_automation_busy(|| coordinator.approve_control_handoff(1, &review, &decision)),
            Err(Error::Revoked | Error::Unauthorized)
        ));
        assert!(coordinator.control_handoffs.is_empty());
    }

    #[test]
    fn direct_input_review_requires_all_enrollments_and_rejects_moved_source() {
        use festerm_automation::Error;
        use presentation::DirectAction;

        let (mut app, first_tab) = FesTermApp::automation_local_fixture();
        let (mut donor, second_tab) = FesTermApp::automation_local_fixture();
        app.adopt_tab(donor.detach_tab(second_tab).unwrap(), None);
        let mut coordinator = fixture(&[&app]);
        coordinator.synthetic_enrollment_metadata_ui_qualified = true;
        let receive = add_peer(&mut coordinator, 1);
        let ime = Arc::new(festerm_session::AutomationEpoch::new());
        ime.transition(true);
        app.set_automation_ime_epoch(&ime);
        let mut bindings = Vec::new();
        for (index, tab) in [first_tab, second_tab].into_iter().enumerate() {
            call(
                &mut coordinator,
                1,
                &[&app],
                "festerm_controller_register",
                json!({"local_id": format!("cross-consent-{index}")}),
            );
            response(&receive);
            let registration = coordinator.peers[&1]
                .controller
                .registrations
                .values()
                .map(|record| record.internal)
                .find(|registration| !bindings.contains(registration))
                .unwrap();
            let target = coordinator.targets[&tab.chip_id()].reference;
            let token = review(
                &mut coordinator,
                &[&app],
                WindowId::PRIMARY,
                1,
                registration,
                target,
            );
            confirm(
                &mut coordinator,
                &[&app],
                WindowId::PRIMARY,
                registration,
                token,
            );
            let enrollment = coordinator.peers[&1]
                .controller
                .registrations
                .values()
                .find(|record| record.internal == registration)
                .unwrap()
                .association
                .unwrap()
                .enrollment;
            approve_metadata_for_delivery(
                &mut coordinator,
                &app,
                1,
                registration,
                enrollment,
                Duration::from_secs(60),
            );
            bindings.push(registration);
        }
        let target = coordinator.targets[&first_tab.chip_id()].reference;
        let other = coordinator.targets[&second_tab.chip_id()].reference;
        let mut sources = Scope {
            discover: BTreeSet::from([target]),
            ..Scope::default()
        };
        assert!(matches!(
            retry_automation_busy(|| coordinator.prepare_direct_action_review_with_sources(
                1,
                target,
                DirectAction::Text {
                    text: "hello",
                    submit: false,
                },
                Duration::from_secs(60),
                app.automation_session_mut(first_tab),
                &sources,
            )),
            Err(Error::Unauthorized)
        ));
        sources.discover.insert(other);
        let review = retry_automation_busy(|| {
            coordinator.prepare_direct_action_review_with_sources(
                1,
                target,
                DirectAction::Text {
                    text: "hello",
                    submit: false,
                },
                Duration::from_secs(60),
                app.automation_session_mut(first_tab),
                &sources,
            )
        })
        .unwrap();
        assert_eq!(review.metadata_sources(), &BTreeSet::from([target, other]));
        assert_eq!(review.enrollment_metadata_grants().len(), 2);
        let decision = review.accept_from_native();
        coordinator.controller_target_moving(second_tab);
        assert!(matches!(
            retry_automation_busy(|| coordinator.approve_control_handoff(1, &review, &decision)),
            Err(Error::Revoked | Error::Unauthorized | Error::StaleTarget)
        ));
        assert!(coordinator.control_handoffs.is_empty());
    }

    #[test]
    fn enrollment_metadata_refresh_charges_only_its_bounded_projection() {
        let (mut coordinator, mut app, tab, registration, enrollment, receive) =
            metadata_fixture_with_frames();
        coordinator.synthetic_enrollment_metadata_ui_qualified = true;
        coordinator.synthetic_enrollment_metadata_delivery_qualified = true;
        let label = "\0".repeat(128);
        app.automation_session_mut(tab).label = label.clone();
        approve_metadata_for_delivery(
            &mut coordinator,
            &app,
            1,
            registration,
            enrollment,
            Duration::from_secs(60),
        );
        let handle = metadata_handle(&coordinator, registration);
        for _ in 0..33 {
            call(
                &mut coordinator,
                1,
                &[&app],
                "festerm_sessions_get",
                json!({"session_ref": handle}),
            );
            let frame = receive().unwrap();
            assert!(frame.is_valid());
            assert_eq!(
                frame_response(&frame)["result"]["structuredContent"]["data"]["sessions"][0]
                    ["label"],
                label
            );
        }
    }

    #[test]
    fn enrollment_metadata_route_keeps_identical_labels_independent() {
        let (mut app, first_tab) = FesTermApp::automation_local_fixture();
        let (mut donor, second_tab) = FesTermApp::automation_local_fixture();
        app.automation_session_mut(first_tab).label = "identical".into();
        donor.automation_session_mut(second_tab).label = "identical".into();
        app.adopt_tab(donor.detach_tab(second_tab).unwrap(), None);
        let mut coordinator = fixture(&[&app]);
        coordinator.synthetic_enrollment_metadata_ui_qualified = true;
        coordinator.synthetic_enrollment_metadata_delivery_qualified = true;
        let receive = add_peer(&mut coordinator, 1);
        let mut bindings = Vec::new();
        for (index, tab) in [first_tab, second_tab].into_iter().enumerate() {
            call(
                &mut coordinator,
                1,
                &[&app],
                "festerm_controller_register",
                json!({"local_id": format!("route-{index}")}),
            );
            response(&receive);
            let registration = coordinator.peers[&1]
                .controller
                .registrations
                .values()
                .map(|record| record.internal)
                .find(|registration| {
                    !bindings
                        .iter()
                        .any(|(previous, _, _)| previous == registration)
                })
                .unwrap();
            let target = coordinator.targets[&tab.chip_id()].reference;
            let token = review(
                &mut coordinator,
                &[&app],
                WindowId::PRIMARY,
                1,
                registration,
                target,
            );
            confirm(
                &mut coordinator,
                &[&app],
                WindowId::PRIMARY,
                registration,
                token,
            );
            let association = coordinator.peers[&1]
                .controller
                .registrations
                .values()
                .find(|record| record.internal == registration)
                .unwrap()
                .association
                .unwrap();
            bindings.push((
                registration,
                association.enrollment,
                association.public.to_wire(),
            ));
        }
        approve_metadata_for_delivery(
            &mut coordinator,
            &app,
            1,
            bindings[0].0,
            bindings[0].1,
            Duration::from_secs(60),
        );
        for (index, (_, _, handle)) in bindings.iter().enumerate() {
            call(
                &mut coordinator,
                1,
                &[&app],
                "festerm_sessions_get",
                json!({"session_ref": handle}),
            );
            let data = response(&receive);
            if index == 0 {
                assert_eq!(
                    data["result"]["structuredContent"]["data"]["sessions"][0]["session_ref"],
                    *handle
                );
                assert_eq!(
                    data["result"]["structuredContent"]["data"]["sessions"][0]["label"],
                    "identical"
                );
            } else {
                assert_eq!(
                    data["result"]["structuredContent"]["code"],
                    "NOT_AUTHORIZED"
                );
            }
        }
        approve_metadata_for_delivery(
            &mut coordinator,
            &app,
            1,
            bindings[1].0,
            bindings[1].1,
            Duration::from_secs(60),
        );
        retry_automation_busy(|| {
            coordinator.revoke_enrollment_metadata_grant(1, bindings[0].0, bindings[0].1)
        })
        .unwrap();
        for (index, (_, _, handle)) in bindings.iter().enumerate() {
            call(
                &mut coordinator,
                1,
                &[&app],
                "festerm_sessions_get",
                json!({"session_ref": handle}),
            );
            let data = response(&receive);
            if index == 0 {
                assert_eq!(
                    data["result"]["structuredContent"]["code"],
                    "NOT_AUTHORIZED"
                );
            } else {
                assert_eq!(
                    data["result"]["structuredContent"]["data"]["sessions"][0]["session_ref"],
                    *handle
                );
            }
        }
    }

    #[test]
    fn enrollment_metadata_route_scopes_colliding_handles_to_each_connection() {
        let (mut app, first_tab) = FesTermApp::automation_local_fixture();
        let (mut donor, second_tab) = FesTermApp::automation_local_fixture();
        app.automation_session_mut(first_tab).label = "first".into();
        donor.automation_session_mut(second_tab).label = "second".into();
        app.adopt_tab(donor.detach_tab(second_tab).unwrap(), None);
        let mut coordinator = fixture(&[&app]);
        coordinator.synthetic_enrollment_metadata_ui_qualified = true;
        coordinator.synthetic_enrollment_metadata_delivery_qualified = true;
        let first_receive = add_peer(&mut coordinator, 1);
        let second_receive = add_peer(&mut coordinator, 2);
        let mut shared_handle = None;
        for (channel, tab, receive) in [
            (1, first_tab, &first_receive),
            (2, second_tab, &second_receive),
        ] {
            call(
                &mut coordinator,
                channel,
                &[&app],
                "festerm_controller_register",
                json!({"local_id": "colliding-id"}),
            );
            response(receive);
            let registration = row(&coordinator, &[&app], channel).registration;
            let target = coordinator.targets[&tab.chip_id()].reference;
            let token = review(
                &mut coordinator,
                &[&app],
                WindowId::PRIMARY,
                channel,
                registration,
                target,
            );
            native(
                &mut coordinator,
                &[&app],
                WindowId::PRIMARY,
                NativeCommand::ConfirmController {
                    channel,
                    registration,
                    selection: token,
                },
            );
            let record = coordinator
                .peers
                .get_mut(&channel)
                .unwrap()
                .controller
                .registrations
                .values_mut()
                .find(|record| record.internal == registration)
                .unwrap();
            let association = record.association.as_mut().unwrap();
            if let Some(handle) = shared_handle {
                association.public = handle;
            } else {
                shared_handle = Some(association.public);
            }
            let enrollment = association.enrollment;
            if channel == 2 {
                call(
                    &mut coordinator,
                    channel,
                    &[&app],
                    "festerm_sessions_get",
                    json!({"session_ref": shared_handle.unwrap().to_wire()}),
                );
                assert_eq!(
                    response(receive)["result"]["structuredContent"]["code"],
                    "NOT_AUTHORIZED"
                );
            }
            approve_metadata_for_delivery(
                &mut coordinator,
                &app,
                channel,
                registration,
                enrollment,
                Duration::from_secs(60),
            );
        }
        for (channel, expected, receive) in
            [(1, "first", &first_receive), (2, "second", &second_receive)]
        {
            call(
                &mut coordinator,
                channel,
                &[&app],
                "festerm_sessions_get",
                json!({"session_ref": shared_handle.unwrap().to_wire()}),
            );
            assert_eq!(
                response(receive)["result"]["structuredContent"]["data"]["sessions"][0]["label"],
                expected
            );
        }
    }

    #[test]
    fn enrollment_metadata_queued_reply_is_invalidated_by_expiry_attachment_or_rebind() {
        for change in ["expiry", "attachment", "rebind"] {
            let (mut coordinator, app, tab, registration, enrollment, receive) =
                metadata_fixture_with_frames();
            coordinator.synthetic_enrollment_metadata_ui_qualified = true;
            coordinator.synthetic_enrollment_metadata_delivery_qualified = true;
            let lifetime = if change == "expiry" {
                Duration::from_secs(1)
            } else {
                Duration::from_secs(60)
            };
            approve_metadata_for_delivery(
                &mut coordinator,
                &app,
                1,
                registration,
                enrollment,
                lifetime,
            );
            let handle = metadata_handle(&coordinator, registration);
            call(
                &mut coordinator,
                1,
                &[&app],
                "festerm_sessions_get",
                json!({"session_ref": handle}),
            );
            let frame = receive().unwrap();
            assert!(frame.is_valid(), "{change}");
            match change {
                "expiry" => {
                    std::thread::sleep(
                        frame.expires.saturating_duration_since(Instant::now())
                            + Duration::from_millis(1),
                    );
                }
                "attachment" => {
                    let Some(crate::tabs::ApplicationSession::TestLocal(backend)) =
                        app.automation_session(tab).unwrap().controller.session()
                    else {
                        panic!("expected synthetic local attachment");
                    };
                    backend.replace_attachment_without_event();
                }
                "rebind" => {
                    let target = coordinator.targets[&tab.chip_id()].reference;
                    let token = review(
                        &mut coordinator,
                        &[&app],
                        WindowId::PRIMARY,
                        1,
                        registration,
                        target,
                    );
                    confirm(
                        &mut coordinator,
                        &[&app],
                        WindowId::PRIMARY,
                        registration,
                        token,
                    );
                    assert_ne!(metadata_handle(&coordinator, registration), handle);
                }
                _ => unreachable!(),
            }
            assert!(!frame.is_valid(), "{change}");
            call(
                &mut coordinator,
                1,
                &[&app],
                "festerm_sessions_get",
                json!({"session_ref": handle}),
            );
            assert_eq!(
                frame_response(&receive().unwrap())["result"]["structuredContent"]["code"],
                "NOT_AUTHORIZED",
                "{change}"
            );
        }
    }

    #[test]
    fn enrollment_metadata_route_preserves_legacy_observation_get() {
        let (mut coordinator, app, tab, _, _, receive) = metadata_fixture_with_frames();
        coordinator.synthetic_enrollment_metadata_ui_qualified = true;
        coordinator.synthetic_enrollment_metadata_delivery_qualified = true;
        let target = coordinator.targets[&tab.chip_id()].reference;
        super::super::tests::approve_for_test(&mut coordinator, 1, vec![target]).unwrap();
        let reference = serde_json::to_string(&target).unwrap();
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_sessions_get",
            json!({"session_ref": reference}),
        );
        let frame = receive().unwrap();
        assert!(frame.is_valid());
        assert!(frame.disclosure.is_none());
        assert_eq!(
            frame_response(&frame)["result"]["structuredContent"]["data"]["sessions"][0]
                ["session_ref"],
            reference
        );
    }

    #[test]
    fn native_enrollment_metadata_review_binds_window_and_explicit_confirmation() {
        let (mut coordinator, app, tab, registration, enrollment) = metadata_fixture();
        let principal = coordinator.peers[&1].principal;
        let target = coordinator.targets[&tab.chip_id()].reference;
        let open = NativeCommand::ReviewControllerMetadata {
            channel: 1,
            registration,
            enrollment,
        };
        assert_eq!(
            metadata_command_with_retry(&mut coordinator, &app, WindowId::PRIMARY, open.clone()),
            Err(CommandError::Refused(Refusal::Unavailable))
        );
        assert!(coordinator.pending_enrollment_metadata_reviews.is_empty());
        coordinator.synthetic_enrollment_metadata_ui_qualified = true;
        assert_eq!(
            metadata_command_with_retry(
                &mut coordinator,
                &app,
                WindowId::for_test(2),
                open.clone(),
            ),
            Err(CommandError::Refused(Refusal::NotAuthorized))
        );
        metadata_command_with_retry(&mut coordinator, &app, WindowId::PRIMARY, open).unwrap();
        let token = metadata_review_token(&coordinator, &app, registration);
        let review_snapshot = stable_settings(&coordinator, &[&app]);
        assert!(coordinator
            .broker
            .as_ref()
            .unwrap()
            .native_enrollment_metadata_grant(principal, registration, enrollment, Instant::now())
            .is_err());
        let confirm = NativeCommand::ConfirmControllerMetadata {
            channel: 1,
            registration,
            review: token,
        };
        assert_eq!(
            metadata_command_with_retry(
                &mut coordinator,
                &app,
                WindowId::for_test(2),
                confirm.clone(),
            ),
            Err(CommandError::Refused(Refusal::NotAuthorized))
        );
        let _other = add_peer(&mut coordinator, 2);
        assert_eq!(
            metadata_command_with_retry(
                &mut coordinator,
                &app,
                WindowId::PRIMARY,
                NativeCommand::ConfirmControllerMetadata {
                    channel: 2,
                    registration,
                    review: token,
                },
            ),
            Err(CommandError::Refused(Refusal::NotAuthorized))
        );
        metadata_command_with_retry(&mut coordinator, &app, WindowId::PRIMARY, confirm).unwrap();
        assert!(!review_snapshot.controllers[0].metadata_review_is_current());
        assert_eq!(
            coordinator
                .broker
                .as_ref()
                .unwrap()
                .native_enrollment_metadata_grant(
                    principal,
                    registration,
                    enrollment,
                    Instant::now(),
                )
                .unwrap()
                .target(),
            target
        );
        assert!(!app.automation_javascript_enabled());
        assert!(coordinator.peers[&1].grant.is_none());
    }

    #[test]
    fn combined_catalog_does_not_expose_a_native_metadata_grant() {
        fn assert_unavailable(
            coordinator: &mut Coordinator,
            app: &FesTermApp,
            receive: &impl Fn() -> Option<Vec<u8>>,
            association: &str,
            target: &str,
        ) {
            call(
                coordinator,
                1,
                &[app],
                "festerm_controller_capabilities",
                json!({}),
            );
            assert_eq!(
                response(receive)["result"]["structuredContent"]["data"],
                json!({
                    "enrollment_available": true,
                    "metadata_available": false,
                    "metadata_requires_separate_grant": true,
                    "presentation_available": false,
                    "input_available": false
                })
            );
            call(
                coordinator,
                1,
                &[app],
                "festerm_presentation_capabilities",
                json!({}),
            );
            let capabilities = response(receive);
            let data = &capabilities["result"]["structuredContent"]["data"];
            assert_eq!(data["enrollment_available"], true);
            assert_eq!(data["presentation"]["available"], false);
            assert_eq!(data["input_available"], false);
            for reference in [association, target] {
                call(
                    coordinator,
                    1,
                    &[app],
                    "festerm_sessions_get",
                    json!({"session_ref": reference}),
                );
                assert_eq!(
                    response(receive)["result"]["structuredContent"]["code"],
                    "NOT_AUTHORIZED"
                );
            }
            call(
                coordinator,
                1,
                &[app],
                "festerm_workspace_present",
                json!({"enrollment_ref": association}),
            );
            assert_eq!(
                response(receive)["result"]["structuredContent"]["code"],
                "UNAVAILABLE"
            );
        }

        let (app, tab) = FesTermApp::automation_local_fixture();
        let mut coordinator = fixture(&[&app]);
        let receive = add_peer(&mut coordinator, 1);
        let Event::Reply(tools) = coordinator
            .peers
            .get_mut(&1)
            .unwrap()
            .machine
            .process_value(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
        else {
            panic!("expected combined tool catalog");
        };
        let catalog: Value = serde_json::from_slice(tools.as_bytes()).unwrap();
        assert!(catalog["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| !matches!(
                tool["name"].as_str(),
                Some("festerm_terminal_send_text" | "festerm_terminal_send_keys")
            )));
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_controller_register",
            json!({"local_id": "metadata catalog"}),
        );
        response(&receive);
        let registration = row(&coordinator, &[&app], 1).registration;
        let target = coordinator.targets[&tab.chip_id()].reference;
        let selection = review(
            &mut coordinator,
            &[&app],
            WindowId::PRIMARY,
            1,
            registration,
            target,
        );
        confirm(
            &mut coordinator,
            &[&app],
            WindowId::PRIMARY,
            registration,
            selection,
        );
        let association = coordinator.peers[&1]
            .controller
            .registrations
            .values()
            .find(|record| record.internal == registration)
            .unwrap()
            .association
            .unwrap();
        let enrollment = association.enrollment;
        let association = association.public.to_wire();
        let target = serde_json::to_string(&target).unwrap();
        let principal = coordinator.peers[&1].principal;
        coordinator.synthetic_enrollment_metadata_ui_qualified = true;
        metadata_command_with_retry(
            &mut coordinator,
            &app,
            WindowId::PRIMARY,
            NativeCommand::ReviewControllerMetadata {
                channel: 1,
                registration,
                enrollment,
            },
        )
        .unwrap();
        let token = metadata_review_token(&coordinator, &app, registration);
        assert_unavailable(&mut coordinator, &app, &receive, &association, &target);
        assert_eq!(
            metadata_review_token(&coordinator, &app, registration),
            token
        );
        metadata_command_with_retry(
            &mut coordinator,
            &app,
            WindowId::PRIMARY,
            NativeCommand::ConfirmControllerMetadata {
                channel: 1,
                registration,
                review: token,
            },
        )
        .unwrap();
        assert!(coordinator
            .broker
            .as_ref()
            .unwrap()
            .native_enrollment_metadata_grant(principal, registration, enrollment, Instant::now())
            .is_ok());
        assert_unavailable(&mut coordinator, &app, &receive, &association, &target);
        assert!(!app.automation_javascript_enabled());
        assert!(coordinator.peers[&1].grant.is_none());

        metadata_command_with_retry(
            &mut coordinator,
            &app,
            WindowId::PRIMARY,
            NativeCommand::RevokeControllerMetadata {
                channel: 1,
                registration,
                enrollment,
            },
        )
        .unwrap();
        let mut broker = coordinator.broker.take().unwrap();
        coordinator.reconcile(&mut broker, &[&app]);
        assert!(broker
            .native_enrollment_metadata_grant(principal, registration, enrollment, Instant::now())
            .is_err());
        coordinator.broker = Some(broker);
        assert_unavailable(&mut coordinator, &app, &receive, &association, &target);
    }

    #[test]
    fn native_enrollment_metadata_review_rejects_changed_display() {
        let (mut coordinator, mut app, tab, registration, enrollment) = metadata_fixture();
        coordinator.synthetic_enrollment_metadata_ui_qualified = true;
        metadata_command_with_retry(
            &mut coordinator,
            &app,
            WindowId::PRIMARY,
            NativeCommand::ReviewControllerMetadata {
                channel: 1,
                registration,
                enrollment,
            },
        )
        .unwrap();
        let token = metadata_review_token(&coordinator, &app, registration);
        app.automation_session_mut(tab).label = "changed after review".into();
        assert_eq!(
            metadata_command_with_retry(
                &mut coordinator,
                &app,
                WindowId::PRIMARY,
                NativeCommand::ConfirmControllerMetadata {
                    channel: 1,
                    registration,
                    review: token,
                },
            ),
            Err(CommandError::Refused(Refusal::NotAuthorized))
        );
        let principal = coordinator.peers[&1].principal;
        assert!(coordinator
            .broker
            .as_ref()
            .unwrap()
            .native_enrollment_metadata_grant(principal, registration, enrollment, Instant::now())
            .is_err());
        let mut broker = coordinator.broker.take().unwrap();
        coordinator.reconcile(&mut broker, &[&app]);
        coordinator.broker = Some(broker);
        assert!(coordinator.pending_enrollment_metadata_reviews.is_empty());
        assert!(coordinator.peers[&1].controller.metadata_review.is_none());
    }

    #[test]
    fn native_enrollment_metadata_snapshot_is_disarmed_by_safety_cuts() {
        for cut in 0..5 {
            let (mut coordinator, app, _tab, registration, enrollment) = metadata_fixture();
            coordinator.synthetic_enrollment_metadata_ui_qualified = true;
            metadata_command_with_retry(
                &mut coordinator,
                &app,
                WindowId::PRIMARY,
                NativeCommand::ReviewControllerMetadata {
                    channel: 1,
                    registration,
                    enrollment,
                },
            )
            .unwrap();
            let snapshot = stable_settings(&coordinator, &[&app]);
            assert!(snapshot.controllers[0].metadata_review_is_current());
            match cut {
                0 => snapshot.disable(),
                1 => snapshot.revoke(1),
                2 => snapshot.unregister_controller(1, registration),
                3 => coordinator.stop_mcp(),
                4 => coordinator.fail_closed_on_unwind(),
                _ => unreachable!(),
            }
            assert!(!snapshot.controllers[0].metadata_review_is_current());
        }
    }

    #[test]
    fn native_enrollment_metadata_disconnect_disarms_stale_snapshots() {
        let (mut coordinator, mut app, _tab, registration, enrollment) = metadata_fixture();
        let settings = festerm_config::InterfaceSettings::DEFAULT.with_mcp_enabled(true);
        app.adopt_broadcast_configuration(
            festerm_config::Configuration::empty()
                .with_interface_settings(settings)
                .unwrap(),
        );
        coordinator.synthetic_enrollment_metadata_ui_qualified = true;
        metadata_command_with_retry(
            &mut coordinator,
            &app,
            WindowId::PRIMARY,
            NativeCommand::ReviewControllerMetadata {
                channel: 1,
                registration,
                enrollment,
            },
        )
        .unwrap();
        let snapshot = stable_settings(&coordinator, &[&app]);
        let row = &snapshot.controllers[0];
        assert!(snapshot.controller_metadata_review_is_current(row));
        coordinator.peers[&1].connection.close();
        assert!(!snapshot.controller_metadata_review_is_current(row));
        let context = egui::Context::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        while coordinator.peers.contains_key(&1) {
            coordinator.tick(&context, &[&app]);
            assert!(Instant::now() < deadline, "closed peer was not retired");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!row.metadata_review_is_current());
    }

    #[test]
    fn native_enrollment_metadata_batch_conflicts_stop_before_confirmation() {
        for reverse in [false, true] {
            let (mut coordinator, app, _tab, registration, enrollment) = metadata_fixture();
            coordinator.synthetic_enrollment_metadata_ui_qualified = true;
            metadata_command_with_retry(
                &mut coordinator,
                &app,
                WindowId::PRIMARY,
                NativeCommand::ReviewControllerMetadata {
                    channel: 1,
                    registration,
                    enrollment,
                },
            )
            .unwrap();
            let token = metadata_review_token(&coordinator, &app, registration);
            let snapshot = stable_settings(&coordinator, &[&app]);
            let principal = coordinator.peers[&1].principal;
            let mut commands = vec![
                NativeCommand::ConfirmControllerMetadata {
                    channel: 1,
                    registration,
                    review: token,
                },
                NativeCommand::CancelControllerMetadata {
                    channel: 1,
                    registration,
                    review: token,
                },
            ];
            if reverse {
                commands.reverse();
            }
            assert!(coordinator.commands_for_test(commands, &egui::Context::default(), &[&app]));
            assert!(coordinator.stop.load(Ordering::Acquire));
            assert!(!snapshot.controllers[0].metadata_review_is_current());
            assert!(coordinator
                .broker
                .as_ref()
                .unwrap()
                .native_enrollment_metadata_grant(
                    principal,
                    registration,
                    enrollment,
                    Instant::now(),
                )
                .is_err());
        }
    }

    #[test]
    fn native_enrollment_metadata_cancellation_and_revocation_defer_without_blocking() {
        if super::super::tests::isolate_admission_contention_test(
            "FESTERM_NATIVE_METADATA_CANCEL_CHILD",
            "automation::controller::tests::native_enrollment_metadata_cancellation_and_revocation_defer_without_blocking",
        ) {
            return;
        }
        let (mut coordinator, app, _tab, registration, enrollment) = metadata_fixture();
        coordinator.synthetic_enrollment_metadata_ui_qualified = true;
        let open = NativeCommand::ReviewControllerMetadata {
            channel: 1,
            registration,
            enrollment,
        };
        metadata_command_with_retry(&mut coordinator, &app, WindowId::PRIMARY, open.clone())
            .unwrap();
        let token = metadata_review_token(&coordinator, &app, registration);
        let snapshot = stable_settings(&coordinator, &[&app]);
        let guard = festerm_session::automation_admission_lock().unwrap();
        snapshot.cancel_controller_metadata_review(1, registration, token);
        let cancelled = command(
            &mut coordinator,
            WindowId::PRIMARY,
            NativeCommand::CancelControllerMetadata {
                channel: 1,
                registration,
                review: token,
            },
            &[&app],
        );
        let stale = command(
            &mut coordinator,
            WindowId::PRIMARY,
            NativeCommand::ConfirmControllerMetadata {
                channel: 1,
                registration,
                review: token,
            },
            &[&app],
        );
        let mut broker = coordinator.broker.take().unwrap();
        let deferred = coordinator.try_reconcile(&mut broker, &[&app]);
        coordinator.broker = Some(broker);
        drop(guard);
        assert_eq!(cancelled, Ok(()));
        assert_eq!(stale, Err(CommandError::Refused(Refusal::NotAuthorized)));
        assert!(matches!(
            deferred,
            Err(ReconcileError::Admission(
                festerm_session::AutomationAdmissionTryError::Busy
            ))
        ));
        let mut broker = coordinator.broker.take().unwrap();
        coordinator.reconcile(&mut broker, &[&app]);
        coordinator.broker = Some(broker);
        assert!(coordinator.pending_enrollment_metadata_reviews.is_empty());

        metadata_command_with_retry(&mut coordinator, &app, WindowId::PRIMARY, open).unwrap();
        let token = metadata_review_token(&coordinator, &app, registration);
        metadata_command_with_retry(
            &mut coordinator,
            &app,
            WindowId::PRIMARY,
            NativeCommand::ConfirmControllerMetadata {
                channel: 1,
                registration,
                review: token,
            },
        )
        .unwrap();
        let principal = coordinator.peers[&1].principal;
        let guard = festerm_session::automation_admission_lock().unwrap();
        let revoke = command(
            &mut coordinator,
            WindowId::PRIMARY,
            NativeCommand::RevokeControllerMetadata {
                channel: 1,
                registration,
                enrollment,
            },
            &[&app],
        );
        assert!(coordinator
            .broker
            .as_ref()
            .unwrap()
            .native_enrollment_metadata_grant(principal, registration, enrollment, Instant::now())
            .is_ok());
        drop(guard);
        assert_eq!(revoke, Ok(()));
        let mut broker = coordinator.broker.take().unwrap();
        coordinator.reconcile(&mut broker, &[&app]);
        coordinator.broker = Some(broker);
        assert!(coordinator
            .broker
            .as_ref()
            .unwrap()
            .native_enrollment_metadata_grant(principal, registration, enrollment, Instant::now())
            .is_err());
    }

    #[test]
    fn native_enrollment_metadata_queued_revocation_does_not_follow_rebinding() {
        let (mut coordinator, app, tab, registration, old_enrollment) = metadata_fixture();
        coordinator.synthetic_enrollment_metadata_ui_qualified = true;
        metadata_command_with_retry(
            &mut coordinator,
            &app,
            WindowId::PRIMARY,
            NativeCommand::ReviewControllerMetadata {
                channel: 1,
                registration,
                enrollment: old_enrollment,
            },
        )
        .unwrap();
        let token = metadata_review_token(&coordinator, &app, registration);
        metadata_command_with_retry(
            &mut coordinator,
            &app,
            WindowId::PRIMARY,
            NativeCommand::ConfirmControllerMetadata {
                channel: 1,
                registration,
                review: token,
            },
        )
        .unwrap();
        metadata_command_with_retry(
            &mut coordinator,
            &app,
            WindowId::PRIMARY,
            NativeCommand::RevokeControllerMetadata {
                channel: 1,
                registration,
                enrollment: old_enrollment,
            },
        )
        .unwrap();
        let target = coordinator.targets[&tab.chip_id()].reference;
        let selection = review(
            &mut coordinator,
            &[&app],
            WindowId::PRIMARY,
            1,
            registration,
            target,
        );
        confirm(
            &mut coordinator,
            &[&app],
            WindowId::PRIMARY,
            registration,
            selection,
        );
        let new_enrollment = coordinator.peers[&1]
            .controller
            .registrations
            .values()
            .find(|record| record.internal == registration)
            .unwrap()
            .association
            .unwrap()
            .enrollment;
        assert_ne!(new_enrollment, old_enrollment);
        metadata_command_with_retry(
            &mut coordinator,
            &app,
            WindowId::PRIMARY,
            NativeCommand::ReviewControllerMetadata {
                channel: 1,
                registration,
                enrollment: new_enrollment,
            },
        )
        .unwrap();
        let token = metadata_review_token(&coordinator, &app, registration);
        metadata_command_with_retry(
            &mut coordinator,
            &app,
            WindowId::PRIMARY,
            NativeCommand::ConfirmControllerMetadata {
                channel: 1,
                registration,
                review: token,
            },
        )
        .unwrap();
        let principal = coordinator.peers[&1].principal;
        let mut broker = coordinator.broker.take().unwrap();
        coordinator.reconcile(&mut broker, &[&app]);
        assert!(broker
            .native_enrollment_metadata_grant(
                principal,
                registration,
                old_enrollment,
                Instant::now(),
            )
            .is_err());
        assert!(broker
            .native_enrollment_metadata_grant(
                principal,
                registration,
                new_enrollment,
                Instant::now(),
            )
            .is_ok());
        assert!(coordinator.peers[&1]
            .controller
            .metadata_revocations
            .is_empty());
        coordinator.broker = Some(broker);
    }

    #[test]
    fn enrollment_metadata_unwind_revokes_grant_and_clears_review() {
        let (mut coordinator, app, tab, registration, enrollment) = metadata_fixture();
        let principal = coordinator.peers[&1].principal;
        let target = coordinator.targets[&tab.chip_id()].reference;
        let review = retry_automation_busy(|| {
            coordinator.prepare_enrollment_metadata_review(
                1,
                registration,
                enrollment,
                Duration::from_secs(60),
                &[&app],
            )
        })
        .unwrap();
        retry_automation_busy(|| {
            coordinator.approve_enrollment_metadata_review(
                1,
                &review,
                &review.accept_from_native(),
                &[&app],
            )
        })
        .unwrap();
        let pending = retry_automation_busy(|| {
            coordinator.prepare_enrollment_metadata_review(
                1,
                registration,
                enrollment,
                Duration::from_secs(60),
                &[&app],
            )
        })
        .unwrap();

        let now = Instant::now();
        let broker = coordinator.broker.as_mut().unwrap();
        let run = broker
            .admit_enrollment_metadata_run(
                principal,
                registration,
                enrollment,
                festerm_automation::MetadataRunSpec {
                    parameters: Digest([19; 32]),
                    targets: BTreeSet::from([target]),
                    budget: Budget::default(),
                    deadline: now + Duration::from_secs(30),
                },
                now,
            )
            .unwrap();
        let request = broker
            .prepare_request(
                principal,
                run,
                AuthorityOperation::Discover { max_bytes: 64 },
                now,
            )
            .unwrap();
        assert_eq!(
            broker.discovery_targets(principal, request, now).unwrap(),
            vec![target]
        );
        broker
            .commit_disclosure(principal, request, 8, now)
            .unwrap();
        let liveness = broker.disclosure_liveness(principal, request, now).unwrap();
        assert!(liveness.is_live(principal, Instant::now()));

        let context = egui::Context::default();
        for _ in 0..4 {
            let mut output = context.run_ui(Default::default(), |_| {});
            output.textures_delta.clear();
        }
        context.set_request_repaint_callback(|_| panic!("metadata command repaint"));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            coordinator.command(NativeCommand::ClearWorkflow, &context);
        }));
        assert_eq!(
            result
                .expect_err("metadata command repaint did not panic")
                .downcast_ref::<&str>(),
            Some(&"metadata command repaint")
        );
        assert!(coordinator.privacy_gate.is_closed());
        assert!(coordinator.stop.load(Ordering::Acquire));
        assert!(coordinator.broker.is_none());
        assert!(!liveness.is_live(principal, Instant::now()));

        coordinator.command(NativeCommand::ClearWorkflow, &egui::Context::default());
        assert!(coordinator.enrollment_metadata_grants.is_empty());
        assert!(coordinator.pending_enrollment_metadata_reviews.is_empty());
        assert!(coordinator.broker.is_none());
        assert!(retry_automation_busy(|| {
            coordinator.approve_enrollment_metadata_review(
                1,
                &pending,
                &pending.accept_from_native(),
                &[&app],
            )
        })
        .is_err());
    }

    #[test]
    fn enrollment_metadata_host_keeps_identical_labels_independent() {
        let (mut app, first_tab) = FesTermApp::automation_local_fixture();
        let (mut donor, second_tab) = FesTermApp::automation_local_fixture();
        app.automation_session_mut(first_tab).label = "identical".into();
        donor.automation_session_mut(second_tab).label = "identical".into();
        app.adopt_tab(donor.detach_tab(second_tab).unwrap(), None);
        let mut coordinator = fixture(&[&app]);
        let receive = add_peer(&mut coordinator, 1);
        let mut bindings = Vec::new();
        for (index, tab) in [first_tab, second_tab].into_iter().enumerate() {
            call(
                &mut coordinator,
                1,
                &[&app],
                "festerm_controller_register",
                json!({"local_id": format!("registration-{index}")}),
            );
            response(&receive);
            let registration = coordinator.peers[&1]
                .controller
                .registrations
                .values()
                .map(|record| record.internal)
                .find(|registration| {
                    !bindings
                        .iter()
                        .any(|(previous, _, _)| previous == registration)
                })
                .unwrap();
            let target = coordinator.targets[&tab.chip_id()].reference;
            let token = review(
                &mut coordinator,
                &[&app],
                WindowId::PRIMARY,
                1,
                registration,
                target,
            );
            confirm(
                &mut coordinator,
                &[&app],
                WindowId::PRIMARY,
                registration,
                token,
            );
            let enrollment = coordinator.peers[&1]
                .controller
                .registrations
                .values()
                .find(|record| record.internal == registration)
                .unwrap()
                .association
                .unwrap()
                .enrollment;
            let pending = retry_automation_busy(|| {
                coordinator.prepare_enrollment_metadata_review(
                    1,
                    registration,
                    enrollment,
                    Duration::from_secs(60),
                    &[&app],
                )
            })
            .unwrap();
            assert_eq!(pending.target(), target);
            let decision = pending.accept_from_native();
            retry_automation_busy(|| {
                coordinator.approve_enrollment_metadata_review(1, &pending, &decision, &[&app])
            })
            .unwrap();
            bindings.push((registration, enrollment, target));
        }
        assert_ne!(bindings[0].2, bindings[1].2);
        assert!(!app.automation_javascript_enabled());
        assert!(coordinator.peers[&1].grant.is_none());
        let principal = coordinator.peers[&1].principal;
        for (registration, enrollment, target) in &bindings {
            assert_eq!(
                coordinator
                    .broker
                    .as_ref()
                    .unwrap()
                    .native_enrollment_metadata_grant(
                        principal,
                        *registration,
                        *enrollment,
                        Instant::now(),
                    )
                    .unwrap()
                    .target(),
                *target
            );
        }
        retry_automation_busy(|| {
            coordinator.revoke_enrollment_metadata_grant(1, bindings[0].0, bindings[0].1)
        })
        .unwrap();
        let broker = coordinator.broker.as_ref().unwrap();
        assert!(broker
            .native_enrollment_metadata_grant(
                principal,
                bindings[0].0,
                bindings[0].1,
                Instant::now(),
            )
            .is_err());
        assert!(broker
            .native_enrollment_metadata_grant(
                principal,
                bindings[1].0,
                bindings[1].1,
                Instant::now(),
            )
            .is_ok());
        let guard = festerm_session::automation_admission_lock().unwrap();
        let current = coordinator.enrollment_metadata_binding_is_current_locked(
            1,
            bindings[1].0,
            bindings[1].1,
            &[&app],
            Instant::now(),
            &guard,
        );
        drop(guard);
        assert!(current);
    }

    #[test]
    fn enrollment_metadata_host_rejects_stale_and_cross_connection_decisions() {
        use festerm_automation::Error;

        for change in ["rebind", "attachment", "disconnect"] {
            let (mut coordinator, app, tab, registration, enrollment) = metadata_fixture();
            let _other = add_peer(&mut coordinator, 2);
            let principal = coordinator.peers[&1].principal;
            let target = coordinator.targets[&tab.chip_id()].reference;
            let pending = retry_automation_busy(|| {
                coordinator.prepare_enrollment_metadata_review(
                    1,
                    registration,
                    enrollment,
                    Duration::from_secs(60),
                    &[&app],
                )
            })
            .unwrap();
            let decision = pending.accept_from_native();
            assert_eq!(
                retry_automation_busy(|| coordinator.approve_enrollment_metadata_review(
                    2,
                    &pending,
                    &decision,
                    &[&app],
                )),
                Err(Error::Unauthorized)
            );
            match change {
                "rebind" => {
                    let token = review(
                        &mut coordinator,
                        &[&app],
                        WindowId::PRIMARY,
                        1,
                        registration,
                        target,
                    );
                    confirm(
                        &mut coordinator,
                        &[&app],
                        WindowId::PRIMARY,
                        registration,
                        token,
                    );
                }
                "attachment" => {
                    let Some(crate::tabs::ApplicationSession::TestLocal(backend)) =
                        app.automation_session(tab).unwrap().controller.session()
                    else {
                        panic!("expected synthetic local attachment");
                    };
                    backend.replace_attachment_without_event();
                }
                "disconnect" => coordinator.peers[&1].connection.close(),
                _ => unreachable!(),
            }
            assert!(
                matches!(
                    retry_automation_busy(|| coordinator.approve_enrollment_metadata_review(
                        1,
                        &pending,
                        &decision,
                        &[&app],
                    )),
                    Err(Error::Revoked | Error::Closed)
                ),
                "{change}"
            );
            assert!(coordinator
                .broker
                .as_ref()
                .unwrap()
                .native_enrollment_metadata_grant(
                    principal,
                    registration,
                    enrollment,
                    Instant::now(),
                )
                .is_err());
        }
    }

    #[test]
    fn enrollment_metadata_host_retires_approved_attachment_replacement() {
        let (mut coordinator, app, tab, registration, enrollment) = metadata_fixture();
        let principal = coordinator.peers[&1].principal;
        let pending = retry_automation_busy(|| {
            coordinator.prepare_enrollment_metadata_review(
                1,
                registration,
                enrollment,
                Duration::from_secs(60),
                &[&app],
            )
        })
        .unwrap();
        let decision = pending.accept_from_native();
        retry_automation_busy(|| {
            coordinator.approve_enrollment_metadata_review(1, &pending, &decision, &[&app])
        })
        .unwrap();
        let guard = festerm_session::automation_admission_lock().unwrap();
        let current = coordinator.enrollment_metadata_binding_is_current_locked(
            1,
            registration,
            enrollment,
            &[&app],
            Instant::now(),
            &guard,
        );
        drop(guard);
        assert!(current);
        let Some(crate::tabs::ApplicationSession::TestLocal(backend)) =
            app.automation_session(tab).unwrap().controller.session()
        else {
            panic!("expected synthetic local attachment");
        };
        backend.replace_attachment_without_event();
        let guard = festerm_session::automation_admission_lock().unwrap();
        let current = coordinator.enrollment_metadata_binding_is_current_locked(
            1,
            registration,
            enrollment,
            &[&app],
            Instant::now(),
            &guard,
        );
        drop(guard);
        assert!(!current);
        let mut broker = coordinator.broker.take().unwrap();
        coordinator.reconcile(&mut broker, &[&app]);
        assert!(broker
            .native_enrollment_metadata_grant(principal, registration, enrollment, Instant::now())
            .is_err());
        coordinator.broker = Some(broker);
    }

    #[test]
    fn enrollment_metadata_host_cancellation_and_workspace_change_consume_review() {
        use festerm_automation::Error;

        let (mut coordinator, app, _tab, registration, enrollment) = metadata_fixture();
        let prepare = |coordinator: &mut Coordinator| {
            retry_automation_busy(|| {
                coordinator.prepare_enrollment_metadata_review(
                    1,
                    registration,
                    enrollment,
                    Duration::from_secs(60),
                    &[&app],
                )
            })
            .unwrap()
        };
        let cancelled = prepare(&mut coordinator);
        let decision = cancelled.accept_from_native();
        retry_automation_busy(|| coordinator.cancel_enrollment_metadata_review(1, &cancelled))
            .unwrap();
        assert_eq!(
            retry_automation_busy(|| coordinator.approve_enrollment_metadata_review(
                1,
                &cancelled,
                &decision,
                &[&app],
            )),
            Err(Error::Revoked)
        );
        let stale = prepare(&mut coordinator);
        let other_stale = prepare(&mut coordinator);
        let decision = stale.accept_from_native();
        let other_decision = other_stale.accept_from_native();
        coordinator
            .presentation_workspace
            .as_ref()
            .unwrap()
            .changed();
        let fresh = prepare(&mut coordinator);
        assert_eq!(
            retry_automation_busy(|| coordinator.approve_enrollment_metadata_review(
                1,
                &stale,
                &decision,
                &[&app],
            )),
            Err(Error::Revoked)
        );
        assert_eq!(
            retry_automation_busy(|| coordinator.approve_enrollment_metadata_review(
                1,
                &other_stale,
                &other_decision,
                &[&app],
            )),
            Err(Error::Revoked)
        );
        let fresh_decision = fresh.accept_from_native();
        retry_automation_busy(|| {
            coordinator.approve_enrollment_metadata_review(1, &fresh, &fresh_decision, &[&app])
        })
        .unwrap();
        assert_eq!(
            retry_automation_busy(|| coordinator.approve_enrollment_metadata_review(
                1,
                &fresh,
                &fresh_decision,
                &[&app],
            )),
            Err(Error::Revoked)
        );
    }

    #[test]
    fn enrollment_metadata_stale_reviews_do_not_block_observation() {
        for reconcile_first in [false, true] {
            let (mut coordinator, app, tab, registration, enrollment) = metadata_fixture();
            let target = coordinator.targets[&tab.chip_id()].reference;
            for _ in 0..2 {
                retry_automation_busy(|| {
                    coordinator.prepare_enrollment_metadata_review(
                        1,
                        registration,
                        enrollment,
                        Duration::from_secs(60),
                        &[&app],
                    )
                })
                .unwrap();
            }
            coordinator
                .presentation_workspace
                .as_ref()
                .unwrap()
                .changed();
            if reconcile_first {
                let mut broker = coordinator.broker.take().unwrap();
                coordinator.reconcile(&mut broker, &[&app]);
                coordinator.broker = Some(broker);
                assert!(coordinator.pending_enrollment_metadata_reviews.is_empty());
            }
            super::super::tests::approve_for_test(&mut coordinator, 1, vec![target]).unwrap();
            assert!(coordinator.pending_enrollment_metadata_reviews.is_empty());
            assert!(coordinator.peers[&1].grant.is_some());
        }
    }

    #[test]
    fn enrollment_metadata_host_refuses_unreviewed_control_composition() {
        use festerm_automation::Error;

        let (mut coordinator, mut app, tab, registration, enrollment) = metadata_fixture();
        let principal = coordinator.peers[&1].principal;
        let target = coordinator.targets[&tab.chip_id()].reference;
        let ime = Arc::new(festerm_session::AutomationEpoch::new());
        ime.transition(true);
        app.set_automation_ime_epoch(&ime);
        let mut scope = Scope::default();
        scope.control.insert(target);
        scope.discover.insert(target);
        scope.invoke_workflows = true;
        scope.return_results = true;
        let binding = InvocationBinding {
            api_version: 1,
            invocation: Invocation::Workflow {
                bundle: Digest([1; 32]),
                manifest: Digest([2; 32]),
            },
            parameters: Digest([3; 32]),
        };
        let request = GrantRequest {
            scope,
            bindings: BTreeSet::from([binding]),
            direct_read_calls: BTreeSet::new(),
        };
        let control = retry_automation_busy(|| {
            coordinator.prepare_control_handoff_review(
                1,
                target,
                request.clone(),
                Duration::from_secs(60),
                &mut app.automation_session_mut(tab).controller,
            )
        })
        .unwrap();
        let control_decision = control.accept_from_native();
        retry_automation_busy(|| {
            coordinator.approve_control_handoff(1, &control, &control_decision)
        })
        .unwrap();
        for _ in 0..10 {
            assert!(matches!(
                retry_automation_busy(|| coordinator.prepare_enrollment_metadata_review(
                    1,
                    registration,
                    enrollment,
                    Duration::from_secs(60),
                    &[&app],
                )),
                Err(Error::Unavailable)
            ));
        }
        assert!(coordinator.pending_enrollment_metadata_reviews.is_empty());
        retry_automation_busy(|| coordinator.revoke_control_handoff(1)).unwrap();
        let pending = retry_automation_busy(|| {
            coordinator.prepare_enrollment_metadata_review(
                1,
                registration,
                enrollment,
                Duration::from_secs(60),
                &[&app],
            )
        })
        .unwrap();
        let decision = pending.accept_from_native();
        retry_automation_busy(|| {
            coordinator.approve_enrollment_metadata_review(1, &pending, &decision, &[&app])
        })
        .unwrap();
        let metadata = coordinator
            .broker
            .as_ref()
            .unwrap()
            .native_enrollment_metadata_grant(principal, registration, enrollment, Instant::now())
            .unwrap();
        for _ in 0..10 {
            assert!(matches!(
                retry_automation_busy(|| coordinator.prepare_control_handoff_review(
                    1,
                    target,
                    request.clone(),
                    Duration::from_secs(60),
                    &mut app.automation_session_mut(tab).controller,
                )),
                Err(Error::Unavailable)
            ));
        }
        assert_eq!(
            coordinator
                .broker
                .as_ref()
                .unwrap()
                .native_enrollment_metadata_grant(
                    principal,
                    registration,
                    enrollment,
                    Instant::now(),
                ),
            Ok(metadata)
        );
        retry_automation_busy(|| {
            coordinator.revoke_enrollment_metadata_grant(1, registration, enrollment)
        })
        .unwrap();
        let control = retry_automation_busy(|| {
            coordinator.prepare_control_handoff_review(
                1,
                target,
                request.clone(),
                Duration::from_secs(60),
                &mut app.automation_session_mut(tab).controller,
            )
        })
        .unwrap();
        let control_decision = control.accept_from_native();
        retry_automation_busy(|| {
            coordinator.approve_control_handoff(1, &control, &control_decision)
        })
        .unwrap();
    }

    #[test]
    fn enrollment_metadata_host_never_waits_for_admission() {
        const CHILD: &str = "FESTERM_ENROLLMENT_METADATA_BUSY_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "automation::controller::tests::enrollment_metadata_host_never_waits_for_admission",
                ])
                .env(CHILD, "1")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                if child.try_wait().unwrap().is_some() {
                    let output = child.wait_with_output().unwrap();
                    assert!(
                        output.status.success(),
                        "isolated metadata admission test failed:\n{}\n{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                    assert!(String::from_utf8_lossy(&output.stdout).contains("running 1 test"));
                    return;
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("isolated metadata admission test timed out");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let (mut coordinator, app, _tab, registration, enrollment) = metadata_fixture();
        let pending = retry_automation_busy(|| {
            coordinator.prepare_enrollment_metadata_review(
                1,
                registration,
                enrollment,
                Duration::from_secs(60),
                &[&app],
            )
        })
        .unwrap();
        let decision = pending.accept_from_native();
        let guard = festerm_session::automation_admission_lock().unwrap();
        let prepare = coordinator.prepare_enrollment_metadata_review(
            1,
            registration,
            enrollment,
            Duration::from_secs(60),
            &[&app],
        );
        let approve =
            coordinator.approve_enrollment_metadata_review(1, &pending, &decision, &[&app]);
        drop(guard);
        assert!(matches!(prepare, Err(festerm_automation::Error::Busy)));
        assert_eq!(approve, Err(festerm_automation::Error::Busy));
        retry_automation_busy(|| {
            coordinator.approve_enrollment_metadata_review(1, &pending, &decision, &[&app])
        })
        .unwrap();
    }

    #[test]
    fn controller_busy_review_defers_and_stale_target_is_retired_on_retry() {
        let (mut app, tab) = FesTermApp::automation_local_fixture();
        let settings = festerm_config::InterfaceSettings::DEFAULT.with_mcp_enabled(true);
        app.adopt_broadcast_configuration(
            festerm_config::Configuration::empty()
                .with_interface_settings(settings)
                .unwrap(),
        );
        let mut coordinator = fixture(&[&app]);
        let receive = add_peer(&mut coordinator, 1);
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_controller_register",
            json!({"local_id": "client"}),
        );
        response(&receive);
        let registration = row(&coordinator, &[&app], 1).registration;
        let target = coordinator.targets[&tab.chip_id()].reference;
        let token = review(
            &mut coordinator,
            &[&app],
            WindowId::PRIMARY,
            1,
            registration,
            target,
        );
        let principal = coordinator.peers[&1].principal;
        app.automation_session_mut(tab)
            .controller
            .advance_lifecycle_generation();

        let (held_tx, held_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
        let worker = std::thread::spawn(move || {
            let _guard = festerm_session::automation_admission_lock().unwrap();
            held_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(3));
        });
        held_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let before = Instant::now();
        assert_eq!(
            command(
                &mut coordinator,
                WindowId::PRIMARY,
                NativeCommand::ConfirmController {
                    channel: 1,
                    registration,
                    selection: token,
                },
                &[&app],
            ),
            Err(CommandError::Busy)
        );
        let context = egui::Context::default();
        assert!(!coordinator.commands(
            vec![NativeWindowCommand {
                window: WindowId::PRIMARY,
                command: NativeCommand::ConfirmController {
                    channel: 1,
                    registration,
                    selection: token,
                },
            }],
            &context,
            &[&app],
        ));
        assert_eq!(coordinator.status, BUSY_STATUS);
        coordinator.tick(&context, &[&app]);
        confirm(
            &mut coordinator,
            &[&app],
            WindowId::PRIMARY,
            registration,
            token,
        );
        let incomplete = coordinator.settings(&[&app]).controller_targets_incomplete;
        let deferred = coordinator.reconciliation_deferred;
        let unchanged = coordinator.peers[&1]
            .controller
            .registrations
            .values()
            .all(|record| record.association.is_none());
        let elapsed = before.elapsed();
        let _ = release_tx.send(());
        worker.join().unwrap();
        assert!(elapsed < Duration::from_secs(1));
        assert!(deferred && incomplete && unchanged);

        let deadline = Instant::now() + Duration::from_secs(5);
        while coordinator.reconciliation_deferred {
            coordinator.tick(&context, &[&app]);
            assert!(Instant::now() < deadline, "reconciliation did not retry");
            std::thread::sleep(Duration::from_millis(1));
        }
        confirm(
            &mut coordinator,
            &[&app],
            WindowId::PRIMARY,
            registration,
            token,
        );
        assert!(coordinator
            .broker
            .as_ref()
            .unwrap()
            .controller_registration(principal, registration)
            .is_ok());
        assert!(coordinator.peers[&1]
            .controller
            .registrations
            .values()
            .all(|record| record.association.is_none()));
        assert_ne!(coordinator.targets[&tab.chip_id()].reference, target);
        assert!(coordinator.peers[&1].grant.is_none());
    }

    #[test]
    fn controller_explicit_retry_replaces_contention_status_after_approval() {
        assert_explicit_retry_replaces_contention_status(false);
        assert_explicit_retry_replaces_contention_status(true);
    }

    fn assert_explicit_retry_replaces_contention_status(deferred: bool) {
        let (mut app, tab) = FesTermApp::automation_local_fixture();
        app.adopt_broadcast_configuration(
            festerm_config::Configuration::empty()
                .with_interface_settings(
                    festerm_config::InterfaceSettings::DEFAULT.with_mcp_enabled(true),
                )
                .unwrap(),
        );
        let mut coordinator = fixture(&[&app]);
        let receive = add_peer(&mut coordinator, 1);
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_controller_register",
            json!({"local_id": "client"}),
        );
        response(&receive);
        let registration = row(&coordinator, &[&app], 1).registration;
        let target = coordinator.targets[&tab.chip_id()].reference;
        let token = review(
            &mut coordinator,
            &[&app],
            WindowId::PRIMARY,
            1,
            registration,
            target,
        );
        let confirmation = NativeCommand::ConfirmController {
            channel: 1,
            registration,
            selection: token,
        };
        let (held_tx, held_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
        let worker = std::thread::spawn(move || {
            let _guard = festerm_session::automation_admission_lock().unwrap();
            held_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(3));
        });
        held_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let context = egui::Context::default();
        if deferred {
            coordinator.tick(&context, &[&app]);
            assert!(coordinator.reconciliation_deferred);
        }
        assert!(!coordinator.commands(
            vec![NativeWindowCommand {
                window: WindowId::PRIMARY,
                command: confirmation.clone(),
            }],
            &context,
            &[&app],
        ));
        assert_eq!(
            coordinator.status,
            if deferred {
                RECONCILIATION_PENDING_STATUS
            } else {
                BUSY_STATUS
            }
        );
        assert!(coordinator.peers[&1]
            .controller
            .registrations
            .values()
            .all(|record| record.association.is_none()));
        release_tx.send(()).unwrap();
        worker.join().unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while coordinator.reconciliation_deferred {
            coordinator.tick(&context, &[&app]);
            assert!(Instant::now() < deadline, "reconciliation did not retry");
            std::thread::sleep(Duration::from_millis(1));
        }
        loop {
            assert!(!coordinator.commands(
                vec![NativeWindowCommand {
                    window: WindowId::PRIMARY,
                    command: confirmation.clone(),
                }],
                &context,
                &[&app],
            ));
            if coordinator.peers[&1]
                .controller
                .registrations
                .values()
                .any(|record| record.association.is_some())
            {
                assert_eq!(coordinator.status, ACCEPTED_STATUS);
                break;
            }
            assert_eq!(coordinator.status, BUSY_STATUS);
            assert!(Instant::now() < deadline, "controller retry stayed busy");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            row(&coordinator, &[&app], 1).associated_target,
            Some(target)
        );
    }

    #[test]
    fn controller_cancel_clears_review_while_admission_is_busy() {
        let (app, tab) = FesTermApp::automation_local_fixture();
        let mut coordinator = fixture(&[&app]);
        let receive = add_peer(&mut coordinator, 1);
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_controller_register",
            json!({"local_id": "client"}),
        );
        response(&receive);
        let registration = row(&coordinator, &[&app], 1).registration;
        let target = coordinator.targets[&tab.chip_id()].reference;
        review(
            &mut coordinator,
            &[&app],
            WindowId::PRIMARY,
            1,
            registration,
            target,
        );

        let (held_tx, held_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
        let worker = std::thread::spawn(move || {
            let _guard = festerm_session::automation_admission_lock().unwrap();
            held_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(3));
        });
        held_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let before = Instant::now();
        let result = command(
            &mut coordinator,
            WindowId::PRIMARY,
            NativeCommand::CancelController {
                channel: 1,
                registration,
            },
            &[&app],
        );
        let cleared = coordinator.peers[&1].controller.selection.is_none();
        let elapsed = before.elapsed();
        let _ = release_tx.send(());
        worker.join().unwrap();
        assert!(elapsed < Duration::from_secs(1));
        assert_eq!(result, Ok(()));
        assert!(cleared);
        coordinator.status = RECONCILIATION_PENDING_STATUS;
        assert!(!coordinator.commands(
            vec![NativeWindowCommand {
                window: WindowId::PRIMARY,
                command: NativeCommand::CancelController {
                    channel: 1,
                    registration,
                },
            }],
            &egui::Context::default(),
            &[&app],
        ));
        assert_eq!(coordinator.status, ACCEPTED_STATUS);
    }

    #[test]
    fn controller_quoted_labels_preserve_unicode_escape_spoofing_and_bound_length() {
        let label = "雪 café 🐚 \"shell\" '主' \\u{202e}\n\u{001b}\u{0085}\u{061c}\u{200b}\u{200d}\u{2028}\u{202e}\u{2066}\u{2069}\u{feff}";
        assert_eq!(
            quoted_approval_label(label),
            concat!(
                "雪 café 🐚 \\\"shell\\\" \\'主\\' \\\\u{202e}\\n",
                "\\u{1b}\\u{85}\\u{61c}\\u{200b}\\u{200d}\\u{2028}",
                "\\u{202e}\\u{2066}\\u{2069}\\u{feff}"
            )
        );

        let maximum = "界".repeat(128);
        assert_eq!(quoted_approval_label(&maximum), maximum);
        assert_eq!(
            quoted_approval_label(&format!("{maximum}more")),
            format!("{maximum}...")
        );
        let before_delimiter = "界".repeat(127);
        assert_eq!(
            quoted_approval_label(&format!("{before_delimiter}\"x")),
            format!("{before_delimiter}\\\"...")
        );
    }

    #[test]
    fn controller_label_display_respects_utf8_registration_and_terminal_limits() {
        let (mut app, tab) = FesTermApp::automation_local_fixture();
        app.automation_session_mut(tab).label = "界".repeat(129);
        let apps = [&app];
        let mut coordinator = fixture(&apps);
        let receive = add_peer(&mut coordinator, 1);
        let local_id = "é".repeat(64);
        call(
            &mut coordinator,
            1,
            &apps,
            "festerm_controller_register",
            json!({"local_id": local_id}),
        );
        response(&receive);
        assert_eq!(row(&coordinator, &apps, 1).label, local_id);
        let snapshot = stable_settings(&coordinator, &apps);
        assert!(snapshot.controller_targets[0]
            .label
            .ends_with(&format!("\"{}...\"", "界".repeat(128))));

        call(
            &mut coordinator,
            1,
            &apps,
            "festerm_controller_register",
            json!({"local_id": format!("{local_id}é")}),
        );
        assert_eq!(
            response(&receive)["result"]["structuredContent"]["code"],
            "INVALID_ARGUMENTS"
        );
        assert_eq!(stable_settings(&coordinator, &apps).controllers.len(), 1);
    }

    #[test]
    fn controller_catalog_requires_qualified_native_opt_in_and_resets_on_stop() {
        let (app, _) = FesTermApp::automation_local_fixture();
        let mut coordinator = unqualified_fixture(&[&app]);
        let mut machine = ready_controller_machine(coordinator.machine_for_new_peer());
        let Event::Reply(tools) = machine.process_value(json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/list"
        })) else {
            panic!("expected tool catalog");
        };
        let catalog: Value = serde_json::from_slice(tools.as_bytes()).unwrap();
        let names = catalog["result"]["tools"].as_array().unwrap();
        assert!(names
            .iter()
            .any(|tool| tool["name"] == "festerm_presentation_capabilities"));
        assert!(!names
            .iter()
            .any(|tool| tool["name"] == "festerm_controller_register"));
        native(
            &mut coordinator,
            &[&app],
            WindowId::PRIMARY,
            NativeCommand::EnableControllerEnrollment,
        );
        assert!(!coordinator.controller_opted_in);
        assert!(!stable_settings(&coordinator, &[&app]).controller_opt_in_available);

        coordinator.synthetic_controller_qualified = true;
        native(
            &mut coordinator,
            &[&app],
            WindowId::PRIMARY,
            NativeCommand::EnableControllerEnrollment,
        );
        assert!(coordinator.controller_opted_in);
        let mut machine = ready_controller_machine(coordinator.machine_for_new_peer());
        let Event::Reply(tools) = machine.process_value(json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/list"
        })) else {
            panic!("expected combined tool catalog");
        };
        let catalog: Value = serde_json::from_slice(tools.as_bytes()).unwrap();
        let names = catalog["result"]["tools"].as_array().unwrap();
        assert!(names
            .iter()
            .any(|tool| tool["name"] == "festerm_controller_register"));
        assert!(names
            .iter()
            .any(|tool| tool["name"] == "festerm_presentation_capabilities"));
        assert!(names
            .iter()
            .any(|tool| tool["name"] == "festerm_workspace_present"));

        let receive = add_peer(&mut coordinator, 1);
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_presentation_capabilities",
            json!({}),
        );
        let capabilities = response(&receive);
        let data = &capabilities["result"]["structuredContent"]["data"];
        assert_eq!(data["enrollment_available"], true);
        assert_eq!(data["presentation"]["available"], false);
        assert_eq!(data["input_available"], false);
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_workspace_present",
            json!({"enrollment_ref": "untrusted"}),
        );
        assert_eq!(
            response(&receive)["result"]["structuredContent"]["code"],
            "UNAVAILABLE"
        );
        assert!(coordinator.peers[&1].controller.registrations.is_empty());

        coordinator.synthetic_controller_qualified = false;
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_controller_capabilities",
            json!({}),
        );
        assert_eq!(
            response(&receive)["result"]["structuredContent"]["data"]["enrollment_available"],
            false
        );
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_presentation_capabilities",
            json!({}),
        );
        assert_eq!(
            response(&receive)["result"]["structuredContent"]["data"]["enrollment_available"],
            false
        );
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_controller_register",
            json!({"local_id": "untrusted"}),
        );
        assert_eq!(
            response(&receive)["result"]["structuredContent"]["code"],
            "UNAVAILABLE"
        );
        assert!(coordinator.peers[&1].controller.registrations.is_empty());
        coordinator.synthetic_controller_qualified = true;
        coordinator.controller_opted_in = false;
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_controller_capabilities",
            json!({}),
        );
        assert_eq!(
            response(&receive)["result"]["structuredContent"]["data"]["enrollment_available"],
            false
        );
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_presentation_capabilities",
            json!({}),
        );
        assert_eq!(
            response(&receive)["result"]["structuredContent"]["data"]["enrollment_available"],
            false
        );
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_controller_register",
            json!({"local_id": "untrusted"}),
        );
        assert_eq!(
            response(&receive)["result"]["structuredContent"]["code"],
            "UNAVAILABLE"
        );
        coordinator.controller_opted_in = true;
        coordinator.presentation_workspace = None;
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_controller_capabilities",
            json!({}),
        );
        assert_eq!(
            response(&receive)["result"]["structuredContent"]["data"]["enrollment_available"],
            false
        );
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_presentation_capabilities",
            json!({}),
        );
        assert_eq!(
            response(&receive)["result"]["structuredContent"]["data"]["enrollment_available"],
            false
        );
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_controller_register",
            json!({"local_id": "untrusted"}),
        );
        assert_eq!(
            response(&receive)["result"]["structuredContent"]["code"],
            "UNAVAILABLE"
        );
        coordinator.stop_mcp();
        assert!(!coordinator.controller_opted_in);
        assert!(coordinator.peers.is_empty());
    }

    #[test]
    fn controller_wire_path_uses_exact_native_choice_without_authority_or_javascript() {
        let (mut app, first_tab) = FesTermApp::automation_local_fixture();
        let (mut donor, second_tab) = FesTermApp::automation_local_fixture();
        let label = "same 日本語\n\u{202e}\"shell\\path";
        app.automation_session_mut(first_tab).label = label.into();
        donor.automation_session_mut(second_tab).label = label.into();
        app.adopt_tab(donor.detach_tab(second_tab).unwrap(), None);
        assert!(!app.automation_javascript_enabled());
        let apps = [&app];
        let mut coordinator = fixture(&apps);
        let receive_first = add_peer(&mut coordinator, 1);
        let receive_second = add_peer(&mut coordinator, 2);
        let first = coordinator.targets[&first_tab.chip_id()].reference;
        let second = coordinator.targets[&second_tab.chip_id()].reference;
        assert_ne!(first, second);
        assert_eq!(
            app.automation_session(first_tab).unwrap().label,
            app.automation_session(second_tab).unwrap().label
        );

        call(
            &mut coordinator,
            1,
            &apps,
            "festerm_controller_capabilities",
            json!({}),
        );
        let capabilities = response(&receive_first);
        let capabilities = &capabilities["result"]["structuredContent"]["data"];
        assert_eq!(
            capabilities,
            &json!({
                "enrollment_available": true,
                "metadata_available": false,
                "metadata_requires_separate_grant": true,
                "presentation_available": false,
                "input_available": false
            })
        );
        let untrusted = "client café\n\u{202e}\"hidden\\label";
        call(
            &mut coordinator,
            1,
            &apps,
            "festerm_controller_register",
            json!({"local_id": untrusted}),
        );
        let registered = response(&receive_first);
        let data = &registered["result"]["structuredContent"]["data"];
        let wire = data["registration_id"].as_str().unwrap().to_owned();
        assert_eq!(data["association_id"], Value::Null);
        assert_eq!(data.as_object().unwrap().len(), 2);
        assert!(!registered.to_string().contains("hidden"));
        let first_row = row(&coordinator, &apps, 1);
        assert_eq!(
            first_row.label,
            "client café\\n\\u{202e}\\\"hidden\\\\label"
        );
        assert!(matches!(first_row.selection, ControllerSelectionView::Idle));
        assert!(first_row.associated_target.is_none());
        assert!(coordinator.peers[&1].grant.is_none());

        call(
            &mut coordinator,
            2,
            &apps,
            "festerm_controller_status",
            json!({"registration_id": wire}),
        );
        assert_eq!(
            response(&receive_second)["result"]["structuredContent"]["code"],
            "NOT_AUTHORIZED"
        );
        call(
            &mut coordinator,
            2,
            &apps,
            "festerm_controller_register",
            json!({"local_id": untrusted}),
        );
        let other_wire = response(&receive_second)["result"]["structuredContent"]["data"]
            ["registration_id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(matches!(
            row(&coordinator, &apps, 2).selection,
            ControllerSelectionView::Idle
        ));
        call(
            &mut coordinator,
            1,
            &apps,
            "festerm_controller_register",
            json!({"local_id": untrusted}),
        );
        assert_eq!(
            response(&receive_first)["result"]["structuredContent"]["data"]["registration_id"],
            wire
        );
        let snapshot = stable_settings(&coordinator, &apps);
        assert_eq!(snapshot.controller_targets.len(), 2);
        assert!(snapshot.controller_targets.iter().all(|candidate| {
            candidate
                .label
                .contains("same 日本語\\n\\u{202e}\\\"shell\\\\path")
                && !candidate.label.contains('\n')
                && !candidate.label.contains('\u{202e}')
        }));
        assert_ne!(
            snapshot.controller_targets[0].label,
            snapshot.controller_targets[1].label
        );
        let second_review = review(
            &mut coordinator,
            &apps,
            WindowId::PRIMARY,
            1,
            first_row.registration,
            second,
        );
        confirm(
            &mut coordinator,
            &apps,
            WindowId::PRIMARY,
            first_row.registration,
            second_review,
        );
        assert_eq!(row(&coordinator, &apps, 1).associated_target, Some(second));
        assert!(coordinator.peers[&1].grant.is_none());
        assert!(coordinator
            .broker
            .as_ref()
            .unwrap()
            .native_metadata_grant(coordinator.peers[&1].principal, Instant::now())
            .is_err());
        let captured = coordinator.peers[&1]
            .controller
            .registrations
            .values()
            .next()
            .unwrap()
            .association
            .unwrap()
            .enrollment;
        assert_eq!(
            coordinator
                .broker
                .as_ref()
                .unwrap()
                .resolve_controller_enrollment(
                    coordinator.peers[&1].principal,
                    first_row.registration,
                    captured,
                ),
            Ok(second)
        );
        call(
            &mut coordinator,
            1,
            &apps,
            "festerm_controller_status",
            json!({"registration_id": wire}),
        );
        let status = response(&receive_first);
        let data = &status["result"]["structuredContent"]["data"];
        let association = data["association_id"].as_str().unwrap().to_owned();
        assert_eq!(data.as_object().unwrap().len(), 2);
        assert!(!status.to_string().contains("shell"));
        call(
            &mut coordinator,
            1,
            &apps,
            "festerm_controller_register",
            json!({"local_id": untrusted}),
        );
        let repeated = response(&receive_first);
        assert_eq!(
            repeated["result"]["structuredContent"]["data"]["registration_id"],
            wire
        );
        assert_eq!(
            repeated["result"]["structuredContent"]["data"]["association_id"],
            association
        );
        assert!(matches!(
            row(&coordinator, &apps, 1).selection,
            ControllerSelectionView::Idle
        ));
        call(
            &mut coordinator,
            2,
            &apps,
            "festerm_controller_status",
            json!({"registration_id": wire}),
        );
        let other_status = response(&receive_second);
        let content = &other_status["result"]["structuredContent"];
        if other_wire == wire {
            assert_eq!(content["data"]["association_id"], Value::Null);
        } else {
            assert_eq!(content["code"], "NOT_AUTHORIZED");
        }
        assert!(!other_status.to_string().contains("shell"));
        call(
            &mut coordinator,
            1,
            &apps,
            "festerm_sessions_get",
            json!({"session_ref": association}),
        );
        assert_eq!(
            response(&receive_first)["result"]["structuredContent"]["code"],
            "NOT_AUTHORIZED"
        );
        call(
            &mut coordinator,
            1,
            &apps,
            "festerm_terminal_read_screen",
            json!({"session_ref": association, "max_bytes": 64}),
        );
        assert_eq!(
            response(&receive_first)["result"]["structuredContent"]["code"],
            "NOT_AUTHORIZED"
        );

        super::super::tests::approve_for_test(&mut coordinator, 1, vec![second]).unwrap();
        call(
            &mut coordinator,
            1,
            &apps,
            "festerm_sessions_get",
            json!({"session_ref": association}),
        );
        let after_grant = response(&receive_first);
        assert_eq!(
            after_grant["result"]["structuredContent"]["status"],
            "refused"
        );
        assert!(!after_grant.to_string().contains("shell"));

        let first_review = review(
            &mut coordinator,
            &apps,
            WindowId::PRIMARY,
            1,
            first_row.registration,
            first,
        );
        confirm(
            &mut coordinator,
            &apps,
            WindowId::PRIMARY,
            first_row.registration,
            first_review,
        );
        assert_eq!(row(&coordinator, &apps, 1).associated_target, Some(first));
        assert!(coordinator
            .broker
            .as_ref()
            .unwrap()
            .resolve_controller_enrollment(
                coordinator.peers[&1].principal,
                first_row.registration,
                captured,
            )
            .is_err());
        call(
            &mut coordinator,
            1,
            &apps,
            "festerm_controller_status",
            json!({"registration_id": wire}),
        );
        assert_ne!(
            response(&receive_first)["result"]["structuredContent"]["data"]["association_id"],
            association
        );
    }

    #[test]
    fn controller_review_refuses_stale_attachment_context_and_newer_choice() {
        for change in [
            "attachment",
            "generation",
            "label",
            "window",
            "workspace",
            "newer",
        ] {
            let (mut app, tab) = FesTermApp::automation_local_fixture();
            let mut coordinator = fixture(&[&app]);
            let receive = add_peer(&mut coordinator, 1);
            call(
                &mut coordinator,
                1,
                &[&app],
                "festerm_controller_register",
                json!({"local_id": "client"}),
            );
            response(&receive);
            let registration = row(&coordinator, &[&app], 1).registration;
            let target = coordinator.targets[&tab.chip_id()].reference;
            let token = review(
                &mut coordinator,
                &[&app],
                WindowId::PRIMARY,
                1,
                registration,
                target,
            );
            match change {
                "attachment" => {
                    let Some(crate::tabs::ApplicationSession::TestLocal(backend)) =
                        app.automation_session_mut(tab).controller.session()
                    else {
                        panic!("expected synthetic local attachment");
                    };
                    backend.replace_attachment_without_event();
                }
                "generation" => app
                    .automation_session_mut(tab)
                    .controller
                    .advance_lifecycle_generation(),
                "label" => app.automation_session_mut(tab).label = "changed".into(),
                "window" => app.set_automation_window(WindowId::for_test(1)),
                "workspace" => coordinator
                    .presentation_workspace
                    .as_ref()
                    .unwrap()
                    .changed(),
                "newer" => native(
                    &mut coordinator,
                    &[&app],
                    WindowId::PRIMARY,
                    NativeCommand::ChooseController {
                        channel: 1,
                        registration,
                    },
                ),
                _ => unreachable!(),
            }
            confirm(
                &mut coordinator,
                &[&app],
                WindowId::PRIMARY,
                registration,
                token,
            );
            assert!(
                coordinator.peers[&1]
                    .controller
                    .registrations
                    .values()
                    .all(|record| record.association.is_none()),
                "{change}"
            );
        }
    }

    #[test]
    fn controller_close_recreate_and_backend_reconnect_never_retarget() {
        use crate::tabs::AppCommand;
        use festerm_session::{SessionError, SessionErrorKind, SessionEvent};

        for change in ["close-recreate", "backend-reconnect"] {
            let (mut app, tab) = FesTermApp::automation_local_fixture();
            app.automation_session_mut(tab).label = "identical".into();
            let mut coordinator = fixture(&[&app]);
            let receive = add_peer(&mut coordinator, 1);
            call(
                &mut coordinator,
                1,
                &[&app],
                "festerm_controller_register",
                json!({"local_id": "client"}),
            );
            let wire = response(&receive)["result"]["structuredContent"]["data"]["registration_id"]
                .as_str()
                .unwrap()
                .to_owned();
            let registration = row(&coordinator, &[&app], 1).registration;
            let principal = coordinator.peers[&1].principal;
            let old = coordinator.targets[&tab.chip_id()].reference;
            let old_guard = coordinator.targets[&tab.chip_id()].guard();
            let token = review(
                &mut coordinator,
                &[&app],
                WindowId::PRIMARY,
                1,
                registration,
                old,
            );
            confirm(
                &mut coordinator,
                &[&app],
                WindowId::PRIMARY,
                registration,
                token,
            );
            let association = coordinator.peers[&1]
                .controller
                .registrations
                .values()
                .next()
                .unwrap()
                .association
                .unwrap();
            assert_eq!(association.target, old);
            let pending = review(
                &mut coordinator,
                &[&app],
                WindowId::PRIMARY,
                1,
                registration,
                old,
            );

            let replacement_tab = match change {
                "close-recreate" => {
                    app.dispatch_for_test(AppCommand::CloseTab(tab), &egui::Context::default());
                    assert!(app.automation_session(tab).is_none());
                    let (mut donor, replacement) = FesTermApp::automation_local_fixture();
                    donor.automation_session_mut(replacement).label = "identical".into();
                    app.adopt_tab(donor.detach_tab(replacement).unwrap(), None);
                    replacement
                }
                "backend-reconnect" => {
                    let Some(crate::tabs::ApplicationSession::TestLocal(backend)) =
                        app.automation_session(tab).unwrap().controller.session()
                    else {
                        panic!("expected synthetic local attachment");
                    };
                    let backend = backend.clone();
                    backend.push_event(SessionEvent::Lifecycle(SessionLifecycle::Disconnected(
                        SessionError::new(SessionErrorKind::Input, "synthetic disconnect"),
                    )));
                    assert!(!old_guard.is_valid());
                    app.automation_session_mut(tab)
                        .controller
                        .advance_lifecycle_generation();
                    backend.try_reconnect().unwrap();
                    backend.push_event(SessionEvent::Lifecycle(SessionLifecycle::Running));
                    tab
                }
                _ => unreachable!(),
            };
            assert_eq!(
                app.automation_session(replacement_tab).unwrap().label,
                "identical"
            );
            assert!(!old_guard.is_valid(), "{change}");
            confirm(
                &mut coordinator,
                &[&app],
                WindowId::PRIMARY,
                registration,
                pending,
            );
            call(
                &mut coordinator,
                1,
                &[&app],
                "festerm_controller_status",
                json!({"registration_id": wire}),
            );
            assert_eq!(
                response(&receive)["result"]["structuredContent"]["code"],
                "NOT_AUTHORIZED",
                "{change}"
            );
            assert!(coordinator
                .broker
                .as_ref()
                .unwrap()
                .resolve_controller_enrollment(principal, registration, association.enrollment)
                .is_err());

            let mut broker = coordinator.broker.take().unwrap();
            coordinator.reconcile(&mut broker, &[&app]);
            coordinator.broker = Some(broker);
            let replacement = coordinator.targets[&replacement_tab.chip_id()].reference;
            assert_ne!(old, replacement);
            assert_ne!(old.generation(), replacement.generation());
            if change == "backend-reconnect" {
                assert_eq!(old.session(), replacement.session());
            } else {
                assert_ne!(old.session(), replacement.session());
            }

            call(
                &mut coordinator,
                1,
                &[&app],
                "festerm_controller_register",
                json!({"local_id": "client"}),
            );
            let new_status = response(&receive);
            let new_data = &new_status["result"]["structuredContent"]["data"];
            let new_wire = new_data["registration_id"].as_str().unwrap();
            assert_ne!(new_wire, wire.as_str());
            assert_eq!(new_data["association_id"], Value::Null);
            let new_registration = row(&coordinator, &[&app], 1).registration;
            assert_ne!(new_registration, registration);
            confirm(
                &mut coordinator,
                &[&app],
                WindowId::PRIMARY,
                registration,
                pending,
            );
            assert!(row(&coordinator, &[&app], 1).associated_target.is_none());
            assert!(coordinator.peers[&1].grant.is_none());
            assert!(!app.automation_javascript_enabled());

            let fresh = review(
                &mut coordinator,
                &[&app],
                WindowId::PRIMARY,
                1,
                new_registration,
                replacement,
            );
            confirm(
                &mut coordinator,
                &[&app],
                WindowId::PRIMARY,
                new_registration,
                fresh,
            );
            assert_eq!(
                row(&coordinator, &[&app], 1).associated_target,
                Some(replacement)
            );
            let fresh_association = coordinator.peers[&1]
                .controller
                .registrations
                .values()
                .next()
                .unwrap()
                .association
                .unwrap();
            assert_ne!(association.public, fresh_association.public);
            assert_eq!(
                coordinator
                    .broker
                    .as_ref()
                    .unwrap()
                    .resolve_controller_enrollment(
                        principal,
                        new_registration,
                        fresh_association.enrollment,
                    ),
                Ok(replacement)
            );
            assert!(coordinator
                .broker
                .as_ref()
                .unwrap()
                .resolve_controller_enrollment(principal, new_registration, association.enrollment)
                .is_err());
            call(
                &mut coordinator,
                1,
                &[&app],
                "festerm_controller_status",
                json!({"registration_id": wire}),
            );
            assert_eq!(
                response(&receive)["result"]["structuredContent"]["code"],
                "NOT_AUTHORIZED"
            );
        }
    }

    #[test]
    fn controller_disconnect_privacy_and_unregister_retire_pending_decisions() {
        for change in ["disconnect", "privacy", "unregister"] {
            let (app, tab) = FesTermApp::automation_local_fixture();
            let mut coordinator = fixture(&[&app]);
            let receive = add_peer(&mut coordinator, 1);
            call(
                &mut coordinator,
                1,
                &[&app],
                "festerm_controller_register",
                json!({"local_id": "client"}),
            );
            let wire = response(&receive)["result"]["structuredContent"]["data"]["registration_id"]
                .as_str()
                .unwrap()
                .to_owned();
            let registration = row(&coordinator, &[&app], 1).registration;
            let principal = coordinator.peers[&1].principal;
            let target = coordinator.targets[&tab.chip_id()].reference;
            let token = review(
                &mut coordinator,
                &[&app],
                WindowId::PRIMARY,
                1,
                registration,
                target,
            );
            match change {
                "disconnect" => coordinator.peers[&1].connection.close(),
                "privacy" => coordinator.privacy_gate.close(),
                "unregister" => {
                    call(
                        &mut coordinator,
                        1,
                        &[&app],
                        "festerm_controller_unregister",
                        json!({"registration_id": wire}),
                    );
                    response(&receive);
                }
                _ => unreachable!(),
            }
            confirm(
                &mut coordinator,
                &[&app],
                WindowId::PRIMARY,
                registration,
                token,
            );
            if change == "privacy" {
                coordinator.privacy_lost();
                assert!(coordinator.broker.is_none());
            } else {
                if change == "disconnect" {
                    coordinator.stop_mcp();
                }
                assert!(coordinator
                    .broker
                    .as_ref()
                    .unwrap()
                    .controller_registration(principal, registration)
                    .is_err());
            }
            assert!(coordinator.peers.values().all(|peer| peer
                .controller
                .registrations
                .values()
                .all(|record| record.association.is_none())));
        }
    }

    #[test]
    fn controller_window_ownership_move_and_unregistration_are_fenced() {
        let (mut first, first_tab) = FesTermApp::automation_local_fixture();
        let (mut second, second_tab) = FesTermApp::automation_local_fixture();
        second.set_automation_window(WindowId::for_test(2));
        first.automation_session_mut(first_tab).label = "same".into();
        second.automation_session_mut(second_tab).label = "same".into();
        let mut coordinator = fixture(&[&first, &second]);
        let receive = add_peer(&mut coordinator, 1);
        call(
            &mut coordinator,
            1,
            &[&first, &second],
            "festerm_controller_register",
            json!({"local_id": "client"}),
        );
        let wire = response(&receive)["result"]["structuredContent"]["data"]["registration_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let registration = row(&coordinator, &[&first, &second], 1).registration;
        let first_target = coordinator.targets[&first_tab.chip_id()].reference;
        let second_target = coordinator.targets[&second_tab.chip_id()].reference;
        native(
            &mut coordinator,
            &[&first, &second],
            WindowId::PRIMARY,
            NativeCommand::ChooseController {
                channel: 1,
                registration,
            },
        );
        let ControllerSelectionView::Choosing { token: stale, .. } =
            row(&coordinator, &[&first, &second], 1).selection
        else {
            panic!("expected chooser");
        };
        coordinator.controller_target_moving(first_tab);
        second.adopt_tab(first.detach_tab(first_tab).unwrap(), None);
        coordinator.controller_target_moving(first_tab);
        first.adopt_tab(second.detach_tab(first_tab).unwrap(), None);
        native(
            &mut coordinator,
            &[&first, &second],
            WindowId::PRIMARY,
            NativeCommand::SelectControllerTarget {
                channel: 1,
                registration,
                selection: stale,
                target: first_target,
            },
        );
        assert!(matches!(
            row(&coordinator, &[&first, &second], 1).selection,
            ControllerSelectionView::Idle
        ));
        native(
            &mut coordinator,
            &[&first, &second],
            WindowId::PRIMARY,
            NativeCommand::ChooseController {
                channel: 1,
                registration,
            },
        );
        let ControllerSelectionView::Choosing { token, .. } =
            row(&coordinator, &[&first, &second], 1).selection
        else {
            panic!("expected chooser");
        };
        native(
            &mut coordinator,
            &[&first, &second],
            WindowId::PRIMARY,
            NativeCommand::SelectControllerTarget {
                channel: 1,
                registration,
                selection: token,
                target: second_target,
            },
        );
        assert!(matches!(
            row(&coordinator, &[&first, &second], 1).selection,
            ControllerSelectionView::Idle
        ));
        let token = review(
            &mut coordinator,
            &[&first, &second],
            WindowId::for_test(2),
            1,
            registration,
            second_target,
        );
        confirm(
            &mut coordinator,
            &[&first, &second],
            WindowId::PRIMARY,
            registration,
            token,
        );
        assert!(row(&coordinator, &[&first, &second], 1)
            .associated_target
            .is_none());
        let token = review(
            &mut coordinator,
            &[&first, &second],
            WindowId::PRIMARY,
            1,
            registration,
            first_target,
        );
        confirm(
            &mut coordinator,
            &[&first, &second],
            WindowId::PRIMARY,
            registration,
            token,
        );
        let record = coordinator.peers[&1]
            .controller
            .registrations
            .values()
            .next()
            .unwrap();
        let flag = record.valid.clone();
        let enrollment = record.association.unwrap().enrollment;
        let principal = coordinator.peers[&1].principal;
        coordinator.controller_target_moving(first_tab);
        assert!(!flag.load(Ordering::Acquire));
        assert!(coordinator
            .broker
            .as_ref()
            .unwrap()
            .resolve_controller_enrollment(principal, registration, enrollment)
            .is_err());
        second.adopt_tab(first.detach_tab(first_tab).unwrap(), None);
        call(
            &mut coordinator,
            1,
            &[&first, &second],
            "festerm_controller_status",
            json!({"registration_id": wire}),
        );
        assert_eq!(
            response(&receive)["result"]["structuredContent"]["code"],
            "NOT_AUTHORIZED"
        );

        call(
            &mut coordinator,
            1,
            &[&first, &second],
            "festerm_controller_register",
            json!({"local_id": "client"}),
        );
        let wire = response(&receive)["result"]["structuredContent"]["data"]["registration_id"]
            .as_str()
            .unwrap()
            .to_owned();
        call(
            &mut coordinator,
            1,
            &[&first, &second],
            "festerm_controller_unregister",
            json!({"registration_id": wire}),
        );
        assert_eq!(
            response(&receive)["result"]["structuredContent"]["data"]["removed"],
            true
        );
        assert!(coordinator
            .settings(&[&first, &second])
            .controllers
            .is_empty());
    }

    #[test]
    fn controller_identifier_collisions_and_exhaustion_fail_closed() {
        let (app, _) = FesTermApp::automation_local_fixture();
        let mut coordinator = fixture(&[&app]);
        let _receive = add_peer(&mut coordinator, 1);
        let peer = coordinator.peers.get_mut(&1).unwrap();
        let first = mint_id_with(peer, || Ok([7; 16])).unwrap();
        let mut attempts = 0;
        assert_eq!(
            mint_id_with(peer, || {
                attempts += 1;
                Ok([7; 16])
            }),
            Err(Refusal::Unavailable)
        );
        assert_eq!(attempts, MAX_ID_ATTEMPTS);
        assert!(peer.controller.issued_ids.contains(&first));
        assert_eq!(
            mint_id_with(peer, || Err(Refusal::Unavailable)),
            Err(Refusal::Unavailable)
        );
        for number in 0u16..MAX_ISSUED_IDS as u16 {
            let mut bytes = [0; 16];
            bytes[..2].copy_from_slice(&number.to_be_bytes());
            peer.controller
                .issued_ids
                .insert(ControllerId::from_random_bytes(bytes));
        }
        assert_eq!(
            mint_id_with(peer, || panic!("exhausted IDs must not be generated")),
            Err(Refusal::BudgetExceeded)
        );
    }

    #[test]
    fn controller_cancellation_and_queue_limits_never_approve_a_stale_review() {
        for reverse in [false, true] {
            let (app, tab) = FesTermApp::automation_local_fixture();
            let mut coordinator = fixture(&[&app]);
            let receive = add_peer(&mut coordinator, 1);
            call(
                &mut coordinator,
                1,
                &[&app],
                "festerm_controller_register",
                json!({"local_id": "client"}),
            );
            response(&receive);
            let registration = row(&coordinator, &[&app], 1).registration;
            let target = coordinator.targets[&tab.chip_id()].reference;
            let token = review(
                &mut coordinator,
                &[&app],
                WindowId::PRIMARY,
                1,
                registration,
                target,
            );
            let approval = NativeCommand::ConfirmController {
                channel: 1,
                registration,
                selection: token,
            };
            let cancel = NativeCommand::ClearWorkflow;
            let commands = if reverse {
                vec![cancel, approval]
            } else {
                vec![approval, cancel]
            };
            assert!(coordinator.commands_for_test(commands, &egui::Context::default(), &[&app]));
            assert!(coordinator.status.contains("cancellation blocked"));
            assert!(matches!(
                row(&coordinator, &[&app], 1).selection,
                ControllerSelectionView::Idle
            ));
            confirm(
                &mut coordinator,
                &[&app],
                WindowId::PRIMARY,
                registration,
                token,
            );
            assert!(row(&coordinator, &[&app], 1).associated_target.is_none());
        }

        let (mut app, _) = FesTermApp::automation_local_fixture();
        let mut coordinator = fixture(&[&app]);
        let receive = add_peer(&mut coordinator, 1);
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_controller_register",
            json!({"local_id": "client"}),
        );
        response(&receive);
        let registration = row(&coordinator, &[&app], 1).registration;
        app.set_automation_settings(stable_settings(&coordinator, &[&app]));
        for _ in 0..=MAX_NATIVE_COMMANDS {
            app.queue_automation_command(NativeCommand::ChooseController {
                channel: 1,
                registration,
            });
        }
        let (commands, overflow) = app.take_automation_commands();
        assert!(overflow);
        assert!(commands.is_empty());
        assert!(coordinator.stop.load(Ordering::Acquire));
    }

    #[test]
    fn controller_rejects_nonlocal_backend_and_closes_on_exhaustion() {
        let (mut app, tab) = FesTermApp::automation_fixture();
        app.automation_session_mut(tab).inspector_transport =
            crate::tabs::InspectorTransport::Local { persistence: None };
        let mut coordinator = fixture(&[&app]);
        let receive = add_peer(&mut coordinator, 1);
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_controller_register",
            json!({"local_id": "client"}),
        );
        let wire = response(&receive)["result"]["structuredContent"]["data"]["registration_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let registration = row(&coordinator, &[&app], 1).registration;
        let target = coordinator.targets[&tab.chip_id()].reference;
        native(
            &mut coordinator,
            &[&app],
            WindowId::PRIMARY,
            NativeCommand::ChooseController {
                channel: 1,
                registration,
            },
        );
        let ControllerSelectionView::Choosing { token, .. } =
            row(&coordinator, &[&app], 1).selection
        else {
            panic!("expected chooser");
        };
        assert!(stable_settings(&coordinator, &[&app])
            .controller_targets
            .is_empty());
        native(
            &mut coordinator,
            &[&app],
            WindowId::PRIMARY,
            NativeCommand::SelectControllerTarget {
                channel: 1,
                registration,
                selection: token,
                target,
            },
        );
        assert!(row(&coordinator, &[&app], 1).associated_target.is_none());

        coordinator.peers.get_mut(&1).unwrap().controller.requests = MAX_CONTROLLER_REQUESTS;
        let principal = coordinator.peers[&1].principal;
        call(
            &mut coordinator,
            1,
            &[&app],
            "festerm_controller_status",
            json!({"registration_id": wire}),
        );
        assert!(coordinator.peers[&1].connection.is_closed());
        assert!(coordinator
            .broker
            .as_ref()
            .unwrap()
            .controller_registration(principal, registration)
            .is_err());
        assert!(receive().is_none());
    }

    #[test]
    fn controller_limit_poison_log_panic_closes_peer_before_reporting() {
        if super::super::tests::isolate_poison_panic_test(
            "FESTERM_CONTROLLER_LIMIT_POISON_LOG_PANIC_CHILD",
            "automation::controller::tests::controller_limit_poison_log_panic_closes_peer_before_reporting",
        ) {
            return;
        }
        let (app, _) = FesTermApp::automation_local_fixture();
        let mut coordinator = fixture(&[&app]);
        let receive = add_peer(&mut coordinator, 1);
        let peer = coordinator.peers.get_mut(&1).unwrap();
        peer.controller.requests = MAX_CONTROLLER_REQUESTS;
        let closed = peer.connection.closed_flag();
        assert!(!closed.load(Ordering::Acquire));
        super::super::tests::poison_admission_for_panic_test();

        super::super::tests::assert_poison_log_panics(|| {
            call(
                &mut coordinator,
                1,
                &[&app],
                "festerm_controller_capabilities",
                json!({}),
            )
        });
        assert!(closed.load(Ordering::Acquire));
        assert!(receive().is_none());
    }

    pub(crate) fn reviewed_controller_for_application(
        app: &FesTermApp,
        tab: TabId,
        workspace: &WorkspaceRevision,
    ) -> (
        Coordinator,
        ControllerRegistrationId,
        u64,
        impl Fn() -> Option<Vec<u8>>,
    ) {
        let mut coordinator = fixture_with_workspace(&[app], workspace);
        let receive = add_peer(&mut coordinator, 1);
        call(
            &mut coordinator,
            1,
            &[app],
            "festerm_controller_register",
            json!({"local_id": "synthetic client"}),
        );
        response(&receive);
        let registration = row(&coordinator, &[app], 1).registration;
        let target = coordinator.targets[&tab.chip_id()].reference;
        let token = review(
            &mut coordinator,
            &[app],
            WindowId::PRIMARY,
            1,
            registration,
            target,
        );
        (coordinator, registration, token, receive)
    }
}
