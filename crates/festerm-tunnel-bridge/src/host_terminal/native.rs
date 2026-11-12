//! Native daemon terminal composition; never installs a listener.
use super::*;
use crate::{
    host_control::{OperationAdmission, PolicyGuard, ProtectedPolicySource},
    owner_auth::{OwnerConnectionGuard, VerifiedOwner},
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

struct Worker<A: ProtectedPolicySource> {
    backend: Arc<NativeTerminal<A>>,
    admission: OperationAdmission,
    owner: Arc<VerifiedOwner>,
    request: Envelope<Request>,
    target: HostTarget,
    pending: PendingAttachment,
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

impl<A: ProtectedPolicySource> Worker<A> {
    fn admit(
        &self,
        state: &AdmissionState,
        other: Option<&Envelope<Request>>,
        cleanup: bool,
    ) -> Result<A::Guard, ControlError> {
        let guard = self.backend.policy.current_policy().map_err(control)?;
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
                guard.verifier(),
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
                    guard.verifier(),
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
        let deadline = Instant::now() + DEADLINE;
        let mut action = Some(action);
        loop {
            if self.admission.cancelled() {
                return Err(ControlError::Cancelled);
            }
            let mut attempted = false;
            let apply = || {
                attempted = true;
                action.take().ok_or(ControlError::Storage)?()
            };
            let result = if !self.adopted {
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
            };
            if !attempted && matches!(result, Err(ControlError::Busy)) && Instant::now() < deadline
            {
                // Retry admission only. Once the nonblocking action is called,
                // its result is final: never replay terminal input or adoption.
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            return result;
        }
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
                        if limit_bytes <= HOST_SNAPSHOT_LIMIT =>
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
        let receipt = self
            .backend
            .sessions
            .maintain_lease(
                &self.pending.authority,
                Some(DetachKind::Intentional),
                |state| self.admit(state, Some(request), false),
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
            self.backend.sessions.with_detachment_receipt_authorized(
                receipt,
                |state| self.admit(state, Some(request), false),
                || Ok(()),
            )
        } else {
            self.action(|| Ok(()))
        }
    }

    fn check(&self, heartbeat: bool) -> Result<(), ControlError> {
        self.action(|| Ok(()))?;
        if heartbeat && self.adopted {
            self.backend
                .sessions
                .maintain_lease(&self.pending.authority, None, |state| {
                    self.admit(state, None, false)
                })?;
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
    owner: VerifiedOwner,
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
    owner: VerifiedOwner,
    guard: OwnerConnectionGuard,
    backend: Arc<NativeTerminal<A>>,
    scheduler: OutputScheduler,
) -> Result<(), Error> {
    let permit = Arc::new(permit);
    let owner = Arc::new(owner);
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
            let current = auth_backend.policy.current_policy()?;
            auth_owner.authorize(current.verifier(), &auth_hello, Epoch::new(1)?, None)?;
            let supported = current.verifier().capabilities();
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
            let guard = current_backend.policy.current_policy()?;
            current_owner.authorize(guard.verifier(), &gate, Epoch::new(1)?, None)?;
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
            refused(&mut stream, &request, reason).await?;
            return Err(Error::Refused(reason));
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
        let pending = backend.sessions.attach(
            owner.principal(),
            &target,
            &attachment,
            takeover,
            HOST_SNAPSHOT_LIMIT as u32,
            Arc::new(Wake(worker_wake)),
            |state| {
                let guard = backend.policy.current_policy().map_err(control)?;
                let captured = admission
                    .authorize_current(
                        guard.verifier(),
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
        let terminal = loop {
            let notified = wake.notified();
            let terminal = call(&jobs, |worker| {
                worker.action(|| Ok(worker.pending.session.take_recovered_terminal_deferred()))
            })
            .await?;
            if let Some(terminal) = terminal {
                break terminal;
            }
            tokio::select! {
                biased;
                _ = stream.read_u8() => return Err(Error::Closed),
                result = tokio::time::timeout_at(deadline, notified) => { result.map_err(|_| Error::Deadline)?; }
            }
        };
        let size = TerminalSize::with_pixels(
            terminal.dimensions().columns() as u16,
            terminal.dimensions().rows() as u16,
            0,
            0,
        )
        .map_err(|_| ContractError::Malformed)?;
        let charge = permit.reserve_terminal_bytes(HOST_SNAPSHOT_LIMIT)?;
        let snapshot = encode_snapshot_bounded(&terminal, HOST_SNAPSHOT_LIMIT)?;
        drop(terminal);
        call(&jobs, |worker| worker.action(|| Ok(()))).await?;
        write(&mut stream, SNAPSHOT, &snapshot).await?;
        drop((snapshot, charge));
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
