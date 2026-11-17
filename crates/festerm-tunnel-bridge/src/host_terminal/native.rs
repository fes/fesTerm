//! Native daemon terminal composition; never installs a listener.
use super::*;
use crate::host_control::ConnectionAuthority;
use crate::{
    host_control::{OperationAdmission, PolicyGuard, ProtectedPolicySource},
    owner_auth::OwnerConnectionGuard,
};
use festerm_host_protocol::{
    AdmissionFence, Capabilities, ContractError, DetachKind, Epoch, Refusal,
};
use festerm_session::{
    Session, SessionEvent, SessionEventNotifier, SessionLifecycle, SessionTryReceiveError,
    TerminalSize,
};
use festerm_sessiond::{
    host_control::{
        AdmissionState, AttachmentCancellation, ControlError, DetachmentReceipt, PendingAttachment,
        SessionControl,
    },
    RecoverySyncCommand,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::Instant,
};

pub struct NativeTerminal<A: ProtectedPolicySource> {
    sessions: Arc<SessionControl>,
    policy: Arc<A>,
}

impl<A: ProtectedPolicySource> NativeTerminal<A> {
    pub fn new(sessions: impl Into<Arc<SessionControl>>, policy: Arc<A>) -> Self {
        Self {
            sessions: sessions.into(),
            policy,
        }
    }
}

struct Wake(Arc<tokio::sync::Notify>);
impl SessionEventNotifier for Wake {
    fn notify(&self) {
        self.0.notify_one();
    }
}

struct RecoveredSnapshot {
    terminal: Terminal,
    permit: Arc<festerm_host_protocol::SnapshotPermit>,
}
struct PreparedSnapshot {
    bytes: Vec<u8>,
    size: TerminalSize,
    _permit: Arc<festerm_host_protocol::SnapshotPermit>,
}
impl RecoveredSnapshot {
    fn prepare(self) -> Result<PreparedSnapshot, Error> {
        let size = TerminalSize::with_pixels(
            self.terminal.dimensions().columns() as u16,
            self.terminal.dimensions().rows() as u16,
            0,
            0,
        )
        .map_err(|_| Error::Refused(Refusal::Incompatible))?;
        if self.terminal.scrollback_stats().limit_bytes() > HOST_DECLARED_HISTORY_LIMIT {
            return Err(Error::Refused(Refusal::Incompatible));
        }
        let bytes = encode_snapshot_bounded(&self.terminal, HOST_SNAPSHOT_LIMIT)
            .map_err(|_| Error::Refused(Refusal::Incompatible))?;
        drop(self.terminal);
        Ok(PreparedSnapshot {
            bytes,
            size,
            _permit: self.permit,
        })
    }
}

fn recovery_failure(lifecycle: SessionLifecycle) -> Option<Error> {
    match lifecycle {
        SessionLifecycle::Failed(_)
        | SessionLifecycle::Disconnected(_)
        | SessionLifecycle::Stopped
        | SessionLifecycle::Exited(_) => Some(Error::Refused(Refusal::Incompatible)),
        _ => None,
    }
}

struct Worker<A: ProtectedPolicySource> {
    backend: Arc<NativeTerminal<A>>,
    admission: OperationAdmission,
    owner: Arc<ConnectionAuthority>,
    request: Envelope<Request>,
    target: HostTarget,
    pending: PendingAttachment,
    snapshot_permit: Option<Arc<festerm_host_protocol::SnapshotPermit>>,
    fence: Mutex<Option<AdmissionFence>>,
    adopted: bool,
    intentional: bool,
    detachment: Option<(DetachmentReceipt, Envelope<Request>)>,
}

fn control(error: ContractError) -> ControlError {
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
        ControlError::CommittedButNotPublished => Refusal::OutcomeUncertain,
        ControlError::RecipeConflict | ControlError::OperationConflict => Refusal::RecipeConflict,
        _ => Refusal::Unknown,
    }
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum BusyBoundary {
    CapabilitiesPolicy,
    CapabilitiesReplyBudget,
    AttachPolicy,
    AttachBudget,
    BackendAttach,
}

#[cfg(test)]
impl BusyBoundary {
    fn message(self, reason: Refusal) -> Option<&'static str> {
        if reason != Refusal::Busy {
            return None;
        }
        Some(match self {
            Self::CapabilitiesPolicy => "host terminal busy boundary=capabilities-policy",
            Self::CapabilitiesReplyBudget => {
                "host terminal busy boundary=capabilities-reply-budget"
            }
            Self::AttachPolicy => "host terminal busy boundary=attach-policy",
            Self::AttachBudget => "host terminal busy boundary=attach-budget",
            Self::BackendAttach => "host terminal busy boundary=backend-attach",
        })
    }

    fn report(self, reason: Refusal) {
        if let Some(message) = self.message(reason) {
            eprintln!("{message}");
        }
    }
}

#[cfg(test)]
#[test]
fn terminal_busy_diagnostic_boundaries_are_static_and_do_not_describe_other_refusals() {
    for (boundary, name) in [
        (BusyBoundary::CapabilitiesPolicy, "capabilities-policy"),
        (
            BusyBoundary::CapabilitiesReplyBudget,
            "capabilities-reply-budget",
        ),
        (BusyBoundary::AttachPolicy, "attach-policy"),
        (BusyBoundary::AttachBudget, "attach-budget"),
        (BusyBoundary::BackendAttach, "backend-attach"),
    ] {
        assert_eq!(
            boundary.message(Refusal::Busy).unwrap(),
            format!("host terminal busy boundary={name}")
        );
        assert!(boundary.message(Refusal::Denied).is_none());
    }
}

fn retry_before_effect<T>(
    deadline: Instant,
    cancelled: impl Fn() -> bool,
    mut operation: impl FnMut(&mut bool) -> Result<T, ControlError>,
) -> Result<T, ControlError> {
    loop {
        if cancelled() {
            return Err(ControlError::Cancelled);
        }
        let mut effect_possible = false;
        let result = operation(&mut effect_possible);
        // Reacquire fresh authorization, but never replay input or a lease
        // mutation once its effect may have begun, even if it reports Busy.
        if !effect_possible
            && matches!(result, Err(ControlError::Busy))
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        return result;
    }
}

impl<A: ProtectedPolicySource> Worker<A> {
    fn admit(
        &self,
        state: &AdmissionState,
        other: Option<&Envelope<Request>>,
        cleanup: bool,
    ) -> Result<A::Guard, ControlError> {
        let guard = self
            .backend
            .policy
            .current_policy_for(self.owner.kind())
            .map_err(control)?;
        let cleanup_admission = cleanup.then(|| {
            OperationAdmission::terminal(
                self.owner.clone(),
                self.backend.policy.clone(),
                self.request.clone(),
                Arc::new(AtomicBool::new(false)),
            )
        });
        let admit = cleanup_admission.as_ref().unwrap_or(&self.admission);
        let fresh = admit
            .authorize_current(
                guard.view(),
                &self.request,
                state.grant.as_ref(),
                state.grant_epoch.ok_or(ControlError::Unknown)?,
            )
            .map_err(control)?;
        let mut fence = self.fence.lock().map_err(|_| ControlError::Storage)?;
        if let Some(captured) = fence.as_ref() {
            captured.revalidate(&fresh).map_err(control)?;
        } else {
            *fence = Some(fresh);
        }
        if let Some(request) = other {
            admit
                .authorize_current(
                    guard.view(),
                    request,
                    state.grant.as_ref(),
                    state.grant_epoch.ok_or(ControlError::Unknown)?,
                )
                .map_err(control)?;
        }
        Ok(guard)
    }

    fn action<T>(
        &self,
        action: impl FnOnce() -> Result<T, ControlError>,
    ) -> Result<T, ControlError> {
        let sessions = &self.backend.sessions;
        let mut action = Some(action);
        retry_before_effect(
            Instant::now() + DEADLINE,
            || self.admission.cancelled(),
            |attempted| {
                let apply = || {
                    *attempted = true;
                    action.take().ok_or(ControlError::Storage)?()
                };
                if !self.adopted {
                    sessions.with_pending_attachment_authorized(
                        &self.pending.authority,
                        |state| self.admit(state, None, false),
                        apply,
                    )
                } else {
                    sessions.with_attachment_authorized(
                        &self.pending.authority,
                        |state| self.admit(state, None, false),
                        apply,
                    )
                }
            },
        )
    }

    fn recovery(&mut self) -> Result<Option<Result<PreparedSnapshot, Error>>, ControlError> {
        let (terminal, lifecycle) = self.action(|| {
            Ok((
                self.pending.session.take_recovered_terminal_deferred(),
                self.pending.session.lifecycle(),
            ))
        })?;
        let Some(terminal) = terminal else {
            return Ok(recovery_failure(lifecycle).map(Err));
        };
        let permit = self.snapshot_permit.take().ok_or(ControlError::Storage)?;
        // CPU work stays on this already-admitted blocking actor, with no
        // installation/policy/journal lease held over serialization.
        let prepared = RecoveredSnapshot { terminal, permit }.prepare();
        self.action(|| Ok(()))?;
        Ok(Some(prepared))
    }

    fn event(&self) -> Result<Option<SessionEvent>, ControlError> {
        if self.intentional {
            return Ok(None);
        }
        self.action(|| match self.pending.session.try_recv_event() {
            Ok(event) => Ok(Some(event)),
            Err(SessionTryReceiveError::Empty) => Ok(None),
            Err(SessionTryReceiveError::Closed) => Err(ControlError::Unknown),
        })
    }

    fn output_active(&self) -> Result<bool, ControlError> {
        if self.intentional {
            Ok(false)
        } else {
            self.action(|| Ok(true))
        }
    }

    fn adopt(&self, size: TerminalSize) -> Result<(), ControlError> {
        self.action(|| {
            if !self.pending.session.recovery_protocol_is_authoritative() {
                return Err(ControlError::Invalid);
            }
            self.pending.session.acknowledge_recovery_adoption();
            // Protocol >=2 emits this only after real sole-writer admission.
            // No resize has been sent previously on this native connection.
            self.pending
                .session
                .try_resize(size)
                .map_err(|_| ControlError::Busy)
        })
    }

    fn committed(&mut self) -> Result<Option<TemporaryLease>, ControlError> {
        let lease = self
            .backend
            .sessions
            .attachment_outcome(&self.pending.authority, |state| {
                self.admit(state, None, false)
            })?;
        self.adopted = true;
        self.action(|| Ok(()))?;
        Ok(lease)
    }

    fn input(&self, frame: Frame) -> Result<(), ControlError> {
        self.action(|| match frame.kind {
            INPUT if !frame.bytes.is_empty() => self
                .pending
                .session
                .try_send_input(&frame.bytes)
                .map_err(|_| ControlError::Busy),
            RESIZE => self
                .pending
                .session
                .try_resize(
                    crate::wire::decode_size(&frame.bytes).map_err(|_| ControlError::Invalid)?,
                )
                .map_err(|_| ControlError::Busy),
            SYNC => {
                let sync: Sync =
                    serde_json::from_slice(&frame.bytes).map_err(|_| ControlError::Invalid)?;
                match RecoverySyncCommand::try_from(sync).map_err(|_| ControlError::Invalid)? {
                    RecoverySyncCommand::SetScrollbackLimit { limit_bytes }
                        if limit_bytes <= HOST_SCROLLBACK_SYNC_LIMIT =>
                    {
                        self.pending
                            .session
                            .sync_recovery_scrollback_limit(limit_bytes)
                    }
                    RecoverySyncCommand::SetColorScheme(colors) => {
                        self.pending.session.sync_recovery_color_scheme(colors)
                    }
                    RecoverySyncCommand::MirrorBytes(bytes) => {
                        self.pending.session.sync_recovery_bytes(&bytes)
                    }
                    RecoverySyncCommand::ResetToInitialState => {
                        self.pending.session.sync_recovery_reset()
                    }
                    _ => return Err(ControlError::Invalid),
                }
                .map_err(|_| ControlError::Busy)
            }
            _ => Err(ControlError::Invalid),
        })
    }

    fn detach(&mut self, request: &Envelope<Request>) -> Result<Response, ControlError> {
        let Request::Detach {
            target,
            attachment,
            lease,
            lease_epoch,
            reason: DetachKind::Intentional,
        } = &request.body
        else {
            return Err(ControlError::Invalid);
        };
        let Request::Attach {
            attachment: expected,
            ..
        } = &self.request.body
        else {
            return Err(ControlError::Invalid);
        };
        if target != &self.target
            || attachment != expected
            || lease.as_ref()
                != self
                    .pending
                    .reserved_lease
                    .as_ref()
                    .map(|lease| &lease.lease)
            || *lease_epoch
                != self
                    .pending
                    .reserved_lease
                    .as_ref()
                    .map(|lease| lease.epoch)
        {
            return Err(ControlError::Stale);
        }
        let receipt = retry_before_effect(
            Instant::now() + DEADLINE,
            || self.admission.cancelled(),
            |effect_possible| {
                self.backend.sessions.maintain_lease(
                    &self.pending.authority,
                    Some(DetachKind::Intentional),
                    |state| {
                        let guard = self.admit(state, Some(request), false)?;
                        *effect_possible = true;
                        Ok(guard)
                    },
                )
            },
        )?
        .ok_or(ControlError::Unknown)?;
        self.detachment = Some((receipt, request.clone()));
        self.intentional = true;
        Ok(Response::Detached {
            target: self.target.clone(),
        })
    }

    fn authorize_reply(&self, response: &Envelope<Response>) -> Result<(), ControlError> {
        if matches!(response.body, Response::Detached { .. }) {
            let (receipt, request) = self.detachment.as_ref().ok_or(ControlError::Stale)?;
            response.validate_for(request).map_err(control)?;
            retry_before_effect(
                Instant::now() + DEADLINE,
                || self.admission.cancelled(),
                |effect_possible| {
                    self.backend.sessions.with_detachment_receipt_authorized(
                        receipt,
                        |state| self.admit(state, Some(request), false),
                        || {
                            *effect_possible = true;
                            Ok(())
                        },
                    )
                },
            )
        } else {
            self.action(|| Ok(()))
        }
    }

    fn check(&self, heartbeat: bool) -> Result<(), ControlError> {
        self.action(|| Ok(()))?;
        if heartbeat && self.adopted {
            retry_before_effect(
                Instant::now() + DEADLINE,
                || self.admission.cancelled(),
                |effect_possible| {
                    self.backend
                        .sessions
                        .maintain_lease(&self.pending.authority, None, |state| {
                            let guard = self.admit(state, None, false)?;
                            *effect_possible = true;
                            Ok(guard)
                        })
                },
            )?;
        }
        Ok(())
    }

    fn cleanup(&self) -> Result<(), ControlError> {
        if self.intentional {
            return Ok(());
        }
        let committed = self
            .backend
            .sessions
            .cancel_attachment(&self.pending.authority, |state| {
                self.admit(state, None, true)
            })
            .inspect_err(|error| {
                eprintln!("host terminal cleanup boundary=cancel-attachment control={error:?}");
            })?;
        if matches!(committed, AttachmentCancellation::Committed(_)) {
            self.backend
                .sessions
                .maintain_lease(
                    &self.pending.authority,
                    Some(DetachKind::TransportLost),
                    |state| self.admit(state, None, true),
                )
                .inspect_err(|error| {
                    eprintln!("host terminal cleanup boundary=transport-lost control={error:?}");
                })?;
        }
        Ok(())
    }
}

type Job<A> = Box<dyn FnOnce(&mut Worker<A>) + Send>;

async fn call<A: ProtectedPolicySource, T: Send + 'static>(
    jobs: &flume::Sender<Job<A>>,
    work: impl FnOnce(&mut Worker<A>) -> Result<T, ControlError> + Send + 'static,
) -> Result<T, Error> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    jobs.send_async(Box::new(move |worker| {
        let _ = tx.send(work(worker));
    }))
    .await
    .map_err(|_| Error::Closed)?;
    rx.await
        .map_err(|_| Error::Closed)?
        .map_err(|error| Error::Refused(refusal(error)))
}

struct Lifetime {
    cancelled: Arc<AtomicBool>,
    closed: CancellationToken,
}
impl Drop for Lifetime {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        self.closed.cancel();
    }
}

async fn refused(
    stream: &mut (impl AsyncWrite + Unpin),
    request: &Envelope<Request>,
    reason: Refusal,
) -> Result<(), Error> {
    let response = response_for(request, Response::Refused { reason });
    write(
        stream,
        HOST_REPLY,
        &festerm_host_protocol::encode(&response)?,
    )
    .await
}

fn response_for(request: &Envelope<Request>, body: Response) -> Envelope<Response> {
    let mut response = Envelope::new(request.host.clone(), request.operation.clone(), body);
    response.issuance_epoch = request.issuance_epoch;
    response
}

/// Internal composition only. The runtime supplies `owner` and `guard` from
/// consumed proof on this exact pinned channel. This installs no endpoint.
#[allow(clippy::too_many_arguments)]
pub async fn serve_terminal<S: AsyncRead + AsyncWrite + Unpin, A: ProtectedPolicySource>(
    stream: S,
    permit: ConnectionPermit,
    owner: impl Into<ConnectionAuthority>,
    guard: OwnerConnectionGuard,
    backend: Arc<NativeTerminal<A>>,
    scheduler: OutputScheduler,
    shutdown: CancellationToken,
) -> Result<(), Error> {
    tokio::select! {
        biased;
        _ = shutdown.cancelled() => Err(Error::Cancelled),
        result = serve_inner(stream, permit, owner, guard, backend, scheduler) => result,
    }
}

async fn serve_inner<S: AsyncRead + AsyncWrite + Unpin, A: ProtectedPolicySource>(
    mut stream: S,
    permit: ConnectionPermit,
    owner: impl Into<ConnectionAuthority>,
    guard: OwnerConnectionGuard,
    backend: Arc<NativeTerminal<A>>,
    scheduler: OutputScheduler,
) -> Result<(), Error> {
    let permit = Arc::new(permit);
    let owner = Arc::new(owner.into());
    let _refusal_charge = permit.budgets().reserve(Resource::ControlBytes, 512)?;
    let first = tokio::time::timeout(DEADLINE, read_frame(&mut stream, &permit, true))
        .await
        .map_err(|_| Error::Deadline)??;
    if first.kind != HOST_REQUEST {
        return Err(ContractError::Malformed.into());
    }
    let hello: Envelope<Request> = festerm_host_protocol::decode(&first.bytes)?;
    drop(first);
    if !matches!(hello.body, Request::Capabilities {}) {
        return Err(ContractError::UnsupportedVersion.into());
    }
    let auth_backend = backend.clone();
    let auth_owner = owner.clone();
    let auth_hello = hello.clone();
    let auth_permit = permit.clone();
    let caps = tokio::time::timeout(
        DEADLINE,
        tokio::task::spawn_blocking(move || {
            let _owned = auth_permit;
            let current = auth_backend.policy.current_policy_for(auth_owner.kind())?;
            auth_owner.authorize(current.view(), &auth_hello, Epoch::new(1)?, None)?;
            let supported = current.view().capabilities();
            Ok::<_, ContractError>(Capabilities {
                attach: supported.attach,
                detach: supported.detach,
                temporary: supported.temporary && supported.attach && supported.detach,
                ..Capabilities::default()
            })
        }),
    )
    .await
    .map_err(|_| Error::Deadline)?
    .map_err(|_| Error::Closed)?;
    let caps = match caps {
        Ok(caps) => caps,
        Err(error) => {
            let reason = refusal(control(error));
            #[cfg(test)]
            BusyBoundary::CapabilitiesPolicy.report(reason);
            refused(&mut stream, &hello, reason).await?;
            return Err(Error::Refused(reason));
        }
    };
    let reply = write_contract(
        &mut stream,
        &permit,
        HOST_REPLY,
        &Envelope::new(
            hello.host.clone(),
            hello.operation.clone(),
            Response::Capabilities { capabilities: caps },
        ),
    )
    .await;
    if let Err(Error::Contract(ContractError::Busy)) = reply {
        #[cfg(test)]
        BusyBoundary::CapabilitiesReplyBudget.report(Refusal::Busy);
        refused(&mut stream, &hello, Refusal::Busy).await?;
        return Err(Error::Refused(Refusal::Busy));
    }
    reply?;
    let frame = tokio::time::timeout(DEADLINE, read_frame(&mut stream, &permit, true))
        .await
        .map_err(|_| Error::Deadline)??;
    if frame.kind != HOST_REQUEST {
        return Err(ContractError::Malformed.into());
    }
    let request: Envelope<Request> = festerm_host_protocol::decode(&frame.bytes)?;
    drop(frame);
    let current_backend = backend.clone();
    let current_owner = owner.clone();
    let current_permit = permit.clone();
    let mut gate = request.clone();
    gate.body = Request::Capabilities {};
    gate.issuance_epoch = None;
    let current = tokio::time::timeout(
        DEADLINE,
        tokio::task::spawn_blocking(move || {
            let _owned = current_permit;
            let guard = current_backend
                .policy
                .current_policy_for(current_owner.kind())?;
            current_owner.authorize(guard.view(), &gate, Epoch::new(1)?, None)?;
            Ok::<_, ContractError>(())
        }),
    )
    .await
    .map_err(|_| Error::Deadline)?
    .map_err(|_| Error::Closed)?;
    if let Err(error) = current {
        if matches!(error, ContractError::Denied | ContractError::Stale) {
            return Err(error.into());
        }
        let reason = refusal(control(error));
        #[cfg(test)]
        BusyBoundary::AttachPolicy.report(reason);
        refused(&mut stream, &request, reason).await?;
        return Err(Error::Refused(reason));
    }
    if !caps.attach || !matches!(request.body, Request::Attach { .. }) {
        refused(&mut stream, &request, Refusal::Unsupported).await?;
        return Err(Error::Refused(Refusal::Unsupported));
    }
    let Request::Attach {
        target,
        attachment,
        takeover,
    } = &request.body
    else {
        unreachable!()
    };
    let target = target.clone();
    let attachment = attachment.clone();
    let takeover = *takeover;
    let charges = (|| {
        Ok::<_, ContractError>((
            permit.budgets().reserve(Resource::Attachment, 1)?,
            permit
                .budgets()
                .reserve(Resource::ControlBytes, CONTROL_FRAME_BYTES)?,
        ))
    })();
    let (attachment_charge, _reply_charge) = match charges {
        Ok(charges) => charges,
        Err(error) => {
            let reason = refusal(control(error));
            #[cfg(test)]
            BusyBoundary::AttachBudget.report(reason);
            refused(&mut stream, &request, reason).await?;
            return Err(Error::Refused(reason));
        }
    };
    // Reserve before the blocking actor, native FSA admission and reader allocation.
    let snapshot_permit = match permit.reserve_snapshot() {
        Ok(permit) => Arc::new(permit),
        Err(_) => {
            refused(&mut stream, &request, Refusal::Busy).await?;
            return Err(Error::Refused(Refusal::Busy));
        }
    };
    let cancelled = Arc::new(AtomicBool::new(false));
    let closed = CancellationToken::new();
    let _lifetime = Lifetime {
        cancelled: cancelled.clone(),
        closed: closed.clone(),
    };
    let wake = Arc::new(tokio::sync::Notify::new());
    let admission = OperationAdmission::terminal(
        owner.clone(),
        backend.policy.clone(),
        request.clone(),
        cancelled.clone(),
    );
    let (jobs, commands) = flume::bounded::<Job<A>>(1);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let worker_wake = wake.clone();
    let worker_closed = closed.clone();
    let worker_permit = permit.clone();
    let worker_request = request.clone();
    tokio::task::spawn_blocking(move || {
        let _owned = (guard, attachment_charge, worker_permit);
        let fence = Mutex::<Option<AdmissionFence>>::new(None);
        let pending = backend.sessions.attach_with_recovery_work(
            owner.principal(),
            &target,
            &attachment,
            takeover,
            HOST_SNAPSHOT_LIMIT as u32,
            Arc::new(Wake(worker_wake)),
            Some(snapshot_permit.clone()),
            |state| {
                let guard = backend
                    .policy
                    .current_policy_for(owner.kind())
                    .map_err(control)?;
                let captured = admission
                    .authorize_current(
                        guard.view(),
                        &worker_request,
                        state.grant.as_ref(),
                        state.grant_epoch.ok_or(ControlError::Unknown)?,
                    )
                    .map_err(control)?;
                let mut fence = fence.lock().map_err(|_| ControlError::Storage)?;
                if let Some(previous) = fence.as_ref() {
                    previous.revalidate(&captured).map_err(control)?;
                } else {
                    *fence = Some(captured);
                }
                Ok(guard)
            },
        );
        let pending = match pending {
            Ok(pending) => pending,
            Err(error) => {
                let _ = started_tx.send(Err(error));
                return;
            }
        };
        let mut worker = Worker {
            backend,
            admission,
            owner,
            request: worker_request,
            target,
            pending,
            snapshot_permit: Some(snapshot_permit),
            fence,
            adopted: false,
            intentional: false,
            detachment: None,
        };
        let _ = started_tx.send(Ok(()));
        let mut checked = Instant::now();
        let mut heartbeat = Instant::now();
        let mut authorized = Instant::now();
        while !cancelled.load(Ordering::Acquire) {
            if !worker.intentional && checked.elapsed() >= Duration::from_secs(1) {
                let renew = heartbeat.elapsed() >= Duration::from_secs(20);
                match worker.check(renew) {
                    Ok(()) => {
                        authorized = Instant::now();
                        if renew {
                            heartbeat = Instant::now();
                        }
                    }
                    Err(ControlError::Busy) if authorized.elapsed() < DEADLINE => {}
                    Err(_) => break,
                }
                checked = Instant::now();
            }
            match commands.recv_timeout(Duration::from_millis(100)) {
                Ok(job) => job(&mut worker),
                Err(flume::RecvTimeoutError::Timeout) => {}
                Err(flume::RecvTimeoutError::Disconnected) => break,
            }
        }
        let cleanup_deadline = Instant::now() + DEADLINE;
        let cleanup = loop {
            match worker.cleanup() {
                Err(ControlError::Busy) if Instant::now() < cleanup_deadline => {
                    std::thread::sleep(
                        Duration::from_millis(10)
                            .min(cleanup_deadline.saturating_duration_since(Instant::now())),
                    );
                }
                result => break result,
            }
        };
        if let Err(error) = cleanup {
            // Content-free diagnostic: denial/stale cleanup cannot extend a
            // newer lease. The actual daemon retains its independent expiry.
            eprintln!("host terminal cleanup incomplete: {error}");
        }
        worker_closed.cancel();
    });
    let route = async {
        let started = tokio::select! {
            biased;
            _ = stream.read_u8() => return Err(Error::Closed),
            result = tokio::time::timeout(DEADLINE, started_rx) =>
                result.map_err(|_| Error::Deadline)?.map_err(|_| Error::Closed)?,
        };
        if let Err(error) = started {
            let reason = refusal(error);
            #[cfg(test)]
            BusyBoundary::BackendAttach.report(reason);
            let response = Envelope::new(
                request.host.clone(),
                request.operation.clone(),
                Response::Refused { reason },
            );
            let bytes = festerm_host_protocol::encode(&response)?;
            write(&mut stream, HOST_REPLY, &bytes).await?;
            return Err(Error::Refused(reason));
        }
        let deadline = tokio::time::Instant::now() + DEADLINE;
        let recovery = async {
            loop {
                let notified = wake.notified();
                if let Some(prepared) = call(&jobs, Worker::recovery).await? { break prepared; }
                tokio::select! {
                    biased;
                    _ = stream.read_u8() => return Err(Error::Closed),
                    result = tokio::time::timeout_at(deadline, notified) => { result.map_err(|_| Error::Deadline)?; }
                }
            }
        }.await;
        let prepared = match recovery {
            Ok(value) => value,
            Err(error) => {
                let reason = match error {
                    Error::Refused(reason) => reason,
                    Error::Contract(ContractError::Busy) => Refusal::Busy,
                    _ => Refusal::Unknown,
                };
                // No SNAPSHOT byte yet. Transport errors after write begins
                // below close only; never append a reply to a partial frame.
                refused(&mut stream, &request, reason).await?;
                return Err(error);
            }
        };
        let size = prepared.size;
        write(&mut stream, SNAPSHOT, &prepared.bytes).await?;
        drop(prepared); // bytes first, then the transaction permit
        let frame = tokio::time::timeout_at(deadline, read_frame(&mut stream, &permit, true))
            .await
            .map_err(|_| Error::Deadline)??;
        if frame.kind != ADOPTED {
            return Err(ContractError::Malformed.into());
        }
        drop(frame);
        let adoption = async {
            call(&jobs, move |worker| worker.adopt(size)).await?;
            let mut staged = VecDeque::new();
            loop {
                let notified = wake.notified();
                let charge = permit.reserve_terminal_bytes(CHUNK_LIMIT)?;
                match call(&jobs, |worker| worker.event()).await? {
                    Some(SessionEvent::ResizeApplied(actual)) if actual == size => {
                        drop(charge);
                        break;
                    }
                    Some(SessionEvent::Lifecycle(
                        SessionLifecycle::Disconnected(_)
                        | SessionLifecycle::Failed(_)
                        | SessionLifecycle::Stopped
                        | SessionLifecycle::Exited(_),
                    ))
                    | Some(SessionEvent::Error(_)) => return Err(Error::Closed),
                    Some(event) => staged.push_back((event, charge)),
                    None => {
                        drop(charge);
                        notified.await;
                    }
                }
            }
            let lease = call(&jobs, Worker::committed).await?;
            Ok::<_, Error>((lease, staged))
        };
        let (lease, mut staged) = tokio::select! {
            biased;
            _ = stream.read_u8() => return Err(Error::Closed),
            result = tokio::time::timeout_at(deadline, adoption) => result.map_err(|_| Error::Deadline)??,
        };
        let Request::Attach {
            target, attachment, ..
        } = &request.body
        else {
            unreachable!()
        };
        let attached = Envelope::new(
            request.host.clone(),
            request.operation.clone(),
            Response::Attached {
                target: target.clone(),
                attachment: attachment.clone(),
                lease,
            },
        );
        call(&jobs, |worker| worker.action(|| Ok(()))).await?;
        let bytes = festerm_host_protocol::encode(&attached)?;
        write(&mut stream, HOST_REPLY, &bytes).await?;
        let (mut read, mut output) = tokio::io::split(stream);
        let (control_tx, mut control_rx) = tokio::sync::mpsc::channel::<Envelope<Response>>(1);
        let reader = async {
            loop {
                let frame = read_frame(&mut read, &permit, true).await?;
                if frame.kind == HOST_REQUEST {
                    let request: Envelope<Request> = festerm_host_protocol::decode(&frame.bytes)?;
                    drop(frame);
                    call(&jobs, |worker| worker.action(|| Ok(()))).await?;
                    let response = if !caps.supports(&request.body) {
                        response_for(
                            &request,
                            Response::Refused {
                                reason: Refusal::Unsupported,
                            },
                        )
                    } else {
                        call(&jobs, move |worker| {
                            let body = worker.detach(&request)?;
                            Ok(response_for(&request, body))
                        })
                        .await?
                    };
                    control_tx.send(response).await.map_err(|_| Error::Closed)?;
                    return std::future::pending::<Result<(), Error>>().await;
                }
                call(&jobs, move |worker| worker.input(frame)).await?;
                tokio::task::yield_now().await;
            }
        };
        let connection = ConnectionId::new();
        let writer = async {
            loop {
                let notified = wake.notified();
                if let Ok(response) = control_rx.try_recv() {
                    let checked = response.clone();
                    call(&jobs, move |worker| worker.authorize_reply(&checked)).await?;
                    write(
                        &mut output,
                        HOST_REPLY,
                        &festerm_host_protocol::encode(&response)?,
                    )
                    .await?;
                    return Ok(());
                }
                let next = if let Some(staged) = staged.pop_front() {
                    let active = call(&jobs, |worker| worker.output_active()).await?;
                    active.then_some(staged)
                } else {
                    let charge = permit.reserve_terminal_bytes(CHUNK_LIMIT)?;
                    call(&jobs, |worker| worker.event())
                        .await?
                        .map(|event| (event, charge))
                };
                if let Some((event, _charge)) = next {
                    let frame = match event {
                        SessionEvent::Output(bytes) => Some((OUTPUT, bytes)),
                        SessionEvent::ResizeApplied(size) => {
                            Some((RESIZED, crate::wire::encode_size(size)))
                        }
                        SessionEvent::RecoverySync(bytes) => {
                            let sync = festerm_sessiond::decode_recovery_sync_command(&bytes)
                                .map_err(|_| ContractError::Malformed)?;
                            Some((SYNC, encode_json_bounded(&Sync::from(sync), CHUNK_LIMIT)?))
                        }
                        SessionEvent::Lifecycle(SessionLifecycle::Exited(_)) => {
                            Some((EXITED, vec![]))
                        }
                        SessionEvent::Lifecycle(SessionLifecycle::Disconnected(error))
                            if error.message() == festerm_sessiond::DISPLACED_REASON =>
                        {
                            Some((DISPLACED, vec![Displacer::Unknown.code()]))
                        }
                        SessionEvent::Lifecycle(
                            SessionLifecycle::Disconnected(_)
                            | SessionLifecycle::Failed(_)
                            | SessionLifecycle::Stopped,
                        )
                        | SessionEvent::Error(_) => return Err(Error::Closed),
                        _ => None,
                    };
                    if let Some((kind, bytes)) = frame {
                        validate(kind, bytes.len(), false)?;
                        scheduler.turn(&connection, &closed).await?;
                        if !call(&jobs, |worker| worker.output_active()).await? {
                            continue;
                        }
                        write(&mut output, kind, &bytes).await?;
                        if matches!(kind, EXITED | DISPLACED) {
                            return Ok(());
                        }
                    }
                } else {
                    tokio::select! {
                        biased;
                        response = control_rx.recv() => {
                            let response = response.ok_or(Error::Closed)?;
                            let checked = response.clone();
                            call(&jobs, move |worker| worker.authorize_reply(&checked)).await?;
                            write(&mut output, HOST_REPLY, &festerm_host_protocol::encode(&response)?).await?;
                            return Ok(());
                        }
                        _ = notified => {}
                    }
                }
            }
        };
        tokio::select! { biased; result = reader => result, result = writer => result }
    };
    tokio::select! {
        biased;
        _ = closed.cancelled() => Err(Error::Closed),
        result = route => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn lease_admission_retries_busy_only_before_effect() {
        let mut calls = 0;
        let mut effects = 0;
        let result = retry_before_effect(
            Instant::now() + Duration::from_secs(1),
            || false,
            |effect_possible| {
                calls += 1;
                if calls < 3 {
                    return Err(ControlError::Busy);
                }
                *effect_possible = true;
                effects += 1;
                Ok(42)
            },
        );
        assert_eq!(result, Ok(42));
        assert_eq!(calls, 3);
        assert_eq!(effects, 1);
    }

    #[test]
    fn lease_admission_never_replays_busy_after_possible_effect() {
        let mut calls = 0;
        let result = retry_before_effect(
            Instant::now() + Duration::from_secs(1),
            || false,
            |effect_possible| {
                calls += 1;
                *effect_possible = true;
                Err::<(), _>(ControlError::Busy)
            },
        );
        assert_eq!(result, Err(ControlError::Busy));
        assert_eq!(calls, 1);
    }

    #[test]
    fn lease_admission_never_retries_non_busy_refusals() {
        for error in [
            ControlError::Denied,
            ControlError::Revoked,
            ControlError::Unknown,
            ControlError::Stale,
            ControlError::RecipeConflict,
            ControlError::OperationConflict,
            ControlError::AlreadyExists,
            ControlError::Ended,
            ControlError::Cancelled,
            ControlError::Invalid,
            ControlError::Storage,
            ControlError::CommittedButNotPublished,
        ] {
            for initially_busy in [false, true] {
                let mut calls = 0;
                let result = retry_before_effect(
                    Instant::now() + Duration::from_secs(1),
                    || false,
                    |_| {
                        calls += 1;
                        if initially_busy && calls == 1 {
                            Err(ControlError::Busy)
                        } else {
                            Err::<(), _>(error)
                        }
                    },
                );
                assert_eq!(result, Err(error));
                assert_eq!(calls, if initially_busy { 2 } else { 1 });
            }
        }
    }

    #[test]
    fn lease_admission_cancellation_stops_before_first_or_next_attempt() {
        for initially_cancelled in [false, true] {
            let cancelled = Cell::new(initially_cancelled);
            let mut calls = 0;
            let result = retry_before_effect(
                Instant::now() + Duration::from_secs(1),
                || cancelled.get(),
                |_| {
                    calls += 1;
                    cancelled.set(true);
                    Err::<(), _>(ControlError::Busy)
                },
            );
            assert_eq!(result, Err(ControlError::Cancelled));
            assert_eq!(calls, usize::from(!initially_cancelled));
        }
    }

    #[test]
    fn lease_admission_expired_budget_preserves_busy() {
        let mut calls = 0;
        let result = retry_before_effect(
            Instant::now(),
            || false,
            |_| {
                calls += 1;
                Err::<(), _>(ControlError::Busy)
            },
        );
        assert_eq!(result, Err(ControlError::Busy));
        assert_eq!(calls, 1);
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    #[test]
    fn native_recovery_terminal_failure_is_incompatible_before_snapshot() {
        use festerm_session::{SessionError, SessionErrorKind};
        for lifecycle in [
            SessionLifecycle::Failed(SessionError::new(SessionErrorKind::Output, "never copied")),
            SessionLifecycle::Disconnected(SessionError::new(
                SessionErrorKind::Output,
                "never copied",
            )),
            SessionLifecycle::Stopped,
        ] {
            assert!(matches!(
                recovery_failure(lifecycle),
                Some(Error::Refused(Refusal::Incompatible))
            ));
        }
        assert!(recovery_failure(SessionLifecycle::Starting).is_none());
        assert!(recovery_failure(SessionLifecycle::Running).is_none());
    }
    #[test]
    fn prepared_snapshot_retains_slot_until_frame_drops_and_refuses_incompatible_policy() {
        let budgets = festerm_host_protocol::Budgets::default();
        let client = ConnectionPermit::reserve(&budgets).unwrap();
        let terminal = Terminal::new(festerm_core::Dimensions::new(119, 16).unwrap()).unwrap();
        let prepared = RecoveredSnapshot {
            terminal,
            permit: Arc::new(client.reserve_snapshot().unwrap()),
        }
        .prepare()
        .unwrap();
        assert!(!prepared.bytes.is_empty());
        assert!(matches!(
            client.reserve_snapshot(),
            Err(ContractError::Busy)
        ));
        drop(prepared);
        let oversized = Terminal::with_scrollback_limit(
            festerm_core::Dimensions::new(119, 16).unwrap(),
            HOST_DECLARED_HISTORY_LIMIT + 1,
        )
        .unwrap();
        let encoded = crate::protocol::encode_snapshot(&oversized).unwrap();
        assert!(crate::protocol::decode_snapshot_with_history_limit(
            &encoded,
            HOST_DECLARED_HISTORY_LIMIT
        )
        .is_err());
        // Legacy codec retains its existing declaration policy.
        assert!(crate::protocol::decode_snapshot(&encoded).is_ok());
        let refused = RecoveredSnapshot {
            terminal: oversized,
            permit: Arc::new(client.reserve_snapshot().unwrap()),
        }
        .prepare();
        assert!(matches!(
            refused,
            Err(Error::Refused(Refusal::Incompatible))
        ));
        assert!(client.reserve_snapshot().is_ok());
    }
}
