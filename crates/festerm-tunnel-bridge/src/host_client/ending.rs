use super::*;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Mutex;

/// One confirmed End action, bound to protected authority and one exact native
/// generation. ExplicitEnd has no creation issuance epoch on the wire.
pub struct EndIntent {
    owner: client_context::Issuer,
    reference: SecretReference,
    binding: Binding,
    target: Target,
    operation: OperationId,
    active: AtomicBool,
    issued: AtomicBool,
    resolved: AtomicBool,
    original: Mutex<Option<Arc<originals::OriginalHandle>>>,
    checkpoint: AtomicU8,
}

impl EndIntent {
    pub fn new<'a>(context: impl Into<ClientContextRef<'a>>, target: Target) -> Self {
        let context = context.into();
        Self {
            owner: context.issuer(),
            reference: context.reference(),
            binding: context.binding().clone(),
            target,
            operation: OperationId::new(),
            active: AtomicBool::new(false),
            issued: AtomicBool::new(false),
            resolved: AtomicBool::new(false),
            original: Mutex::new(None),
            checkpoint: AtomicU8::new(0),
        }
    }

    pub fn protect_original(
        &self,
        store: Arc<originals::OriginalStore>,
        review: String,
    ) -> Result<(), Failure> {
        if !store.matches_issuer(&self.owner, &self.reference, &self.binding) {
            return Err(Failure::BindingMismatch);
        }
        let mut original = self.original.lock().map_err(|_| Failure::OriginalStorage)?;
        if let Some(original) = original.as_ref() {
            if !original.agrees(&store, &review) {
                return Err(Failure::BindingMismatch);
            }
        } else {
            if self.is_issued() {
                return Err(Failure::OriginalStorage);
            }
            *original = Some(store.handle(self.operation.to_string(), review));
        }
        Ok(())
    }

    pub(super) fn from_original<'a>(
        context: impl Into<ClientContextRef<'a>>,
        target: Target,
        operation: OperationId,
        handle: Arc<originals::OriginalHandle>,
    ) -> Self {
        let mut intent = Self::new(context, target);
        intent.operation = operation;
        intent.issued.store(true, Ordering::Release);
        intent.checkpoint.store(2, Ordering::Release);
        intent.original = Mutex::new(Some(handle));
        intent
    }

    pub fn matches_context<'a>(&self, context: impl Into<ClientContextRef<'a>>) -> bool {
        let context = context.into();
        self.owner == context.issuer()
            && self.reference == context.reference()
            && &self.binding == context.binding()
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    pub fn operation(&self) -> &OperationId {
        &self.operation
    }

    pub fn is_issued(&self) -> bool {
        self.issued.load(Ordering::Acquire)
    }

    pub fn is_resolved(&self) -> bool {
        self.resolved.load(Ordering::Acquire)
    }

    pub fn definitively_settles(&self, creation: &CreationIntent) -> bool {
        self.is_resolved()
            && creation.settled_by(&self.owner, &self.reference, &self.binding, &self.target)
    }
}

struct Flight<'a>(&'a AtomicBool);

impl Drop for Flight<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[cfg(any(test, feature = "testing"))]
pub(crate) async fn end_authenticated<S: AsyncRead + AsyncWrite + Unpin>(
    client: &mut ControlClient<S>,
    intent: Arc<EndIntent>,
    cancel: &CancellationToken,
) -> Result<Outcome, Failure> {
    let attempt = EndAttempt::new(intent.clone());
    end_observed(client, intent, cancel, &attempt).await
}

pub(super) async fn end_observed<S: AsyncRead + AsyncWrite + Unpin>(
    client: &mut ControlClient<S>,
    intent: Arc<EndIntent>,
    cancel: &CancellationToken,
    attempt: &EndAttempt,
) -> Result<Outcome, Failure> {
    attempt.stage(1);
    if client.binding() != &intent.binding {
        return Err(Failure::BindingMismatch);
    }
    if intent.active.swap(true, Ordering::AcqRel) {
        return Err(ContractError::Busy.into());
    }
    let _flight = Flight(&intent.active);
    if cancel.is_cancelled() {
        return Err(Error::Cancelled.into());
    }
    if intent.is_resolved() {
        return Err(ContractError::Stale.into());
    }
    attempt.stage(2);
    if !client.handshake(cancel).await?.explicit_end {
        return Err(Error::Refused(festerm_host_protocol::Refusal::Unsupported).into());
    }
    // Conservative across cancellation/write failure: the original may have
    // reached the host. A retry never substitutes an operation or target.
    attempt.stage(3);
    let protected = intent
        .original
        .lock()
        .map_err(|_| Failure::OriginalStorage)?
        .clone();
    if let Some(protected) = &protected {
        intent.checkpoint.store(1, Ordering::Release);
        attempt.checkpoint.store(1, Ordering::Release);
        protected.capture_end(&intent.operation, &intent.target)?;
        intent.checkpoint.store(2, Ordering::Release);
        attempt.checkpoint.store(2, Ordering::Release);
    }
    attempt.stage(4);
    intent.issued.store(true, Ordering::Release);
    attempt.issued.store(true, Ordering::Release);
    let response = client
        .request(
            intent.operation.clone(),
            Request::ExplicitEnd {
                target: intent.target.clone(),
            },
            cancel,
        )
        .await?;
    match response.body {
        Response::Ended { target } if target == intent.target => {
            attempt.stage(5);
            if let Some(protected) = &protected {
                intent.checkpoint.store(1, Ordering::Release);
                attempt.checkpoint.store(1, Ordering::Release);
                protected.confirmed_end(&intent.operation, &target)?;
                intent.checkpoint.store(3, Ordering::Release);
                attempt.checkpoint.store(3, Ordering::Release);
            }
            intent.resolved.store(true, Ordering::Release);
            attempt.resolved.store(true, Ordering::Release);
            Ok(Outcome::Ended { target })
        }
        Response::Refused { reason } => Err(Error::Refused(reason).into()),
        _ => Err(ContractError::Malformed.into()),
    }
}

/// Content-free client observations from one End attempt, not host execution proof.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EndFailureDiagnostic {
    pub stage: &'static str,
    pub class: &'static str,
    pub refusal: &'static str,
    pub io_kind: &'static str,
    pub checkpoint: &'static str,
    pub issued: bool,
    pub resolved: bool,
}

/// One worker attempt; never shared by concurrent/repeated uses of an original.
pub(super) struct EndAttempt {
    stage: AtomicU8,
    checkpoint: AtomicU8,
    issued: AtomicBool,
    resolved: AtomicBool,
}
impl EndAttempt {
    pub(super) fn new(intent: Arc<EndIntent>) -> Self {
        Self {
            stage: AtomicU8::new(0),
            checkpoint: AtomicU8::new(intent.checkpoint.load(Ordering::Acquire)),
            issued: AtomicBool::new(intent.is_issued()),
            resolved: AtomicBool::new(intent.is_resolved()),
        }
    }
    pub(super) fn stage(&self, stage: u8) {
        self.stage.store(stage, Ordering::Release);
    }

    /// Finite observations only; issued is not evidence of host delivery.
    pub(super) fn failure_diagnostic(&self, failure: Failure) -> EndFailureDiagnostic {
        let (class, refusal, io_kind) = diagnostics::failure_fields(failure);
        EndFailureDiagnostic {
            stage: match self.stage.load(Ordering::Acquire) {
                0 => "connection",
                1 => "admission",
                2 => "capabilities",
                3 => "original-capture",
                4 => "request",
                _ => "original-confirmation",
            },
            class,
            refusal,
            io_kind,
            checkpoint: match self.checkpoint.load(Ordering::Acquire) {
                0 => "not-observed",
                1 => "unconfirmed",
                2 => "confirmed-present",
                _ => "confirmed-retired",
            },
            issued: self.issued.load(Ordering::Acquire),
            resolved: self.resolved.load(Ordering::Acquire),
        }
    }
}
