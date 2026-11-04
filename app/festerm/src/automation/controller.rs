use super::*;
use festerm_session::SessionLifecycle;

const MAX_ISSUED_IDS: usize = 256;
const MAX_ID_ATTEMPTS: usize = 8;
const MAX_LOCAL_TARGETS: usize = 128;
const MAX_CONTROLLER_REQUESTS: usize = 256;

struct Registration {
    internal: ControllerRegistrationId,
    valid: Arc<AtomicBool>,
    association: Option<Association>,
}

#[derive(Clone, Copy)]
struct Association {
    public: ControllerId,
    enrollment: ControllerEnrollmentId,
    target: TargetRef,
    window: WindowId,
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

#[derive(Default)]
pub(super) struct PeerState {
    registrations: BTreeMap<ControllerId, Registration>,
    issued_ids: BTreeSet<ControllerId>,
    selection: Option<Selection>,
    requests: usize,
    #[cfg(test)]
    next_test_id: Option<u128>,
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
fn remove_locked(broker: &mut Broker, peer: &mut Peer, id: ControllerId) -> bool {
    let Some(record) = peer.controller.registrations.remove(&id) else {
        return false;
    };
    record.valid.store(false, Ordering::Release);
    if peer
        .controller
        .selection
        .as_ref()
        .is_some_and(|selection| selection.registration() == record.internal)
    {
        peer.controller.selection = None;
    }
    let _ = broker.unregister_controller_session(peer.principal, record.internal);
    true
}

fn status_locked(
    broker: &mut Broker,
    targets: &BTreeMap<u64, Target>,
    peer: &mut Peer,
    id: ControllerId,
    apps: &[&FesTermApp],
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
        remove_locked(broker, peer, id);
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
            | Operation::ControllerRegister { .. }
            | Operation::ControllerStatus { .. }
            | Operation::ControllerUnregister { .. }
    )
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
    now: Instant,
) {
    if peer.controller.requests >= MAX_CONTROLLER_REQUESTS {
        peer.connection.close();
        peer.machine.close();
        broker.disconnect(peer.principal);
        peer.controller = PeerState::default();
        return;
    }
    peer.controller.requests += 1;
    let key = request.key;
    let result = if let Some(_guard) = festerm_session::automation_admission_lock() {
        let open =
            !privacy.is_closed() && !stop.load(Ordering::Acquire) && !peer.connection.is_closed();
        match request.operation {
            Operation::ControllerCapabilities => {
                Ok(Reply::plain(BackendResult::ControllerCapabilities {
                    enrollment_available: qualified && open,
                }))
            }
            _ if !open || !qualified => Err(Refusal::Unavailable),
            Operation::ControllerRegister { local_id } => {
                match broker.register_controller_session(peer.principal, local_id.as_bytes()) {
                    Ok(internal) => {
                        let existing = public_registration(peer, internal);
                        let id = match existing {
                            Some(id) => Ok(id),
                            None => mint_id(peer).inspect_err(|_| {
                                let _ =
                                    broker.unregister_controller_session(peer.principal, internal);
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
                                    },
                                );
                            }
                            status_locked(broker, targets, peer, id, apps)
                        })
                    }
                    Err(error) => Err(refusal(error)),
                }
            }
            Operation::ControllerStatus { registration_id } => {
                status_locked(broker, targets, peer, registration_id, apps)
            }
            Operation::ControllerUnregister { registration_id } => {
                if remove_locked(broker, peer, registration_id) {
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
) {
    if peers.is_empty() {
        return;
    }
    let Some(_guard) = festerm_session::automation_admission_lock() else {
        for peer in peers.values() {
            peer.connection.close();
        }
        return;
    };
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
            remove_locked(broker, peer, id);
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
    }
}

fn next_selection(revision: &mut u64) -> Result<u64, Refusal> {
    *revision = revision.checked_add(1).ok_or(Refusal::Unavailable)?;
    Ok(*revision)
}

fn peer_is_open(peer: &Peer, privacy: &PrivacyGate, stop: &AtomicBool) -> bool {
    !peer.connection.is_closed() && !privacy.is_closed() && !stop.load(Ordering::Acquire)
}

pub(super) fn command(
    coordinator: &mut Coordinator,
    window: WindowId,
    action: NativeCommand,
    apps: &[&FesTermApp],
) -> Result<(), Refusal> {
    let Some(_guard) = festerm_session::automation_admission_lock() else {
        return Err(Refusal::Unavailable);
    };
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
                .prepare_controller_enrollment(peer.principal, registration, target)
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
                )
                .map_err(refusal)?;
            if broker
                .resolve_controller_enrollment(peer.principal, registration, enrollment)
                .ok()
                != Some(target)
            {
                remove_locked(broker, peer, id);
                return Err(Refusal::NotAuthorized);
            }
            let record = peer
                .controller
                .registrations
                .get_mut(&id)
                .ok_or(Refusal::NotAuthorized)?;
            record.valid.store(false, Ordering::Release);
            record.valid = Arc::new(AtomicBool::new(true));
            record.association = Some(Association {
                public,
                enrollment,
                target,
                window,
            });
        }
        NativeCommand::CancelController {
            channel,
            registration,
        } => {
            let peer = coordinator.peers.get_mut(&channel).ok_or(Refusal::Closed)?;
            if peer
                .controller
                .selection
                .as_ref()
                .is_some_and(|pending| pending.registration() == registration)
            {
                peer.controller.selection = None;
            }
        }
        NativeCommand::UnregisterController {
            channel,
            registration,
        } => {
            let peer = coordinator.peers.get_mut(&channel).ok_or(Refusal::Closed)?;
            if let Some(id) = public_registration(peer, registration) {
                remove_locked(broker, peer, id);
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
    let Some(_guard) = festerm_session::automation_admission_lock() else {
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
            remove_locked(broker, peer, id);
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
    let Some(_guard) = festerm_session::automation_admission_lock() else {
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
                    Some(ControllerRow {
                        channel: *channel,
                        registration: record.internal,
                        public_id: *public_id,
                        label,
                        associated_target,
                        selection,
                        valid: record.valid.clone(),
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
            let mut broker = Broker::new([0; 16]);
            reconcile(
                &mut broker,
                &BTreeMap::new(),
                &mut BTreeMap::new(),
                &[],
                None,
            );
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

    fn add_peer(coordinator: &mut Coordinator, channel: u64) -> impl Fn() -> Option<Vec<u8>> {
        let principal = coordinator
            .broker
            .as_mut()
            .unwrap()
            .accept_connection()
            .unwrap();
        let (connection, receive) = ipc::test_connection(channel);
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
                controller,
            },
        );
        receive
    }

    fn call(
        coordinator: &mut Coordinator,
        channel: u64,
        apps: &[&FesTermApp],
        name: &str,
        arguments: Value,
    ) {
        let bytes = serde_json::to_vec(&json!({
            "jsonrpc": "2.0", "id": 9, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
        .unwrap();
        let qualified = coordinator.controller_qualified() && coordinator.controller_opted_in;
        process_peer_message(
            coordinator.broker.as_mut().unwrap(),
            &coordinator.targets,
            coordinator.peers.get_mut(&channel).unwrap(),
            &bytes,
            apps,
            &coordinator.privacy_gate,
            &coordinator.stop,
            qualified,
            Instant::now(),
        );
    }

    fn response(receive: &impl Fn() -> Option<Vec<u8>>) -> Value {
        serde_json::from_slice(&receive().expect("expected an outbound response")).unwrap()
    }

    fn row(coordinator: &Coordinator, apps: &[&FesTermApp], channel: u64) -> ControllerRow {
        coordinator
            .settings(apps)
            .controllers
            .into_iter()
            .find(|row| row.channel == channel)
            .expect("registration is visible")
    }

    fn native(
        coordinator: &mut Coordinator,
        apps: &[&FesTermApp],
        window: WindowId,
        command: NativeCommand,
    ) {
        assert!(!coordinator.commands(
            vec![NativeWindowCommand { window, command }],
            &egui::Context::default(),
            apps,
        ));
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
            row(coordinator, apps, channel).selection
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
        } = row(coordinator, apps, channel).selection
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
        let snapshot = coordinator.settings(&apps);
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
        assert_eq!(coordinator.settings(&apps).controllers.len(), 1);
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
        assert!(!coordinator.settings(&[&app]).controller_opt_in_available);

        coordinator.synthetic_controller_qualified = true;
        native(
            &mut coordinator,
            &[&app],
            WindowId::PRIMARY,
            NativeCommand::EnableControllerEnrollment,
        );
        assert!(coordinator.controller_opted_in);
        let receive = add_peer(&mut coordinator, 1);
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
        let snapshot = coordinator.settings(&apps);
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

        coordinator.approve(1, vec![second]).unwrap();
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
            reconcile(
                &mut broker,
                &coordinator.targets,
                &mut coordinator.peers,
                &[&app],
                coordinator
                    .presentation_workspace
                    .as_ref()
                    .and_then(WorkspaceRevision::current),
            );
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
        app.set_automation_settings(coordinator.settings(&[&app]));
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
        assert!(coordinator.settings(&[&app]).controller_targets.is_empty());
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
