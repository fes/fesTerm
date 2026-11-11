use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// One confirmed End action, bound to protected authority and one exact native
/// generation. ExplicitEnd has no creation issuance epoch on the wire.
pub struct EndIntent {
    owner: OwnerBinding,
    reference: SecretReference,
    binding: Binding,
    target: Target,
    operation: OperationId,
    active: AtomicBool,
    issued: AtomicBool,
    resolved: AtomicBool,
    original: Mutex<Option<Arc<originals::OriginalHandle>>>,
}

impl EndIntent {
    pub fn new(context: &ApprovedContext, target: Target) -> Self {
        Self {
            owner: context.approved.clone(),
            reference: context.reference.duplicate_for_transport(),
            binding: context.binding.clone(),
            target,
            operation: OperationId::new(),
            active: AtomicBool::new(false),
            issued: AtomicBool::new(false),
            resolved: AtomicBool::new(false),
            original: Mutex::new(None),
        }
    }

    pub fn protect_original(
        &self,
        store: Arc<originals::OriginalStore>,
        review: String,
    ) -> Result<(), Failure> {
        if !store.matches(&self.owner, &self.reference, &self.binding) {
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

    pub(super) fn from_original(
        context: &ApprovedContext,
        target: Target,
        operation: OperationId,
        handle: Arc<originals::OriginalHandle>,
    ) -> Self {
        let mut intent = Self::new(context, target);
        intent.operation = operation;
        intent.issued.store(true, Ordering::Release);
        intent.original = Mutex::new(Some(handle));
        intent
    }

    pub fn matches_context(&self, context: &ApprovedContext) -> bool {
        self.owner == context.approved
            && self.reference == context.reference
            && self.binding == context.binding
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

pub(crate) async fn end_authenticated<S: AsyncRead + AsyncWrite + Unpin>(
    client: &mut ControlClient<S>,
    intent: Arc<EndIntent>,
    cancel: &CancellationToken,
) -> Result<Outcome, Failure> {
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
    if !client.handshake(cancel).await?.explicit_end {
        return Err(Error::Refused(festerm_host_protocol::Refusal::Unsupported).into());
    }
    // Conservative across cancellation/write failure: the original may have
    // reached the host. A retry never substitutes an operation or target.
    let protected = intent
        .original
        .lock()
        .map_err(|_| Failure::OriginalStorage)?
        .clone();
    if let Some(protected) = &protected {
        protected.capture_end(&intent.operation, &intent.target)?;
    }
    intent.issued.store(true, Ordering::Release);
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
        Response::Ended { target } => {
            if let Some(protected) = &protected {
                protected.confirmed_end(&intent.operation, &target)?;
            }
            intent.resolved.store(true, Ordering::Release);
            Ok(Outcome::Ended { target })
        }
        Response::Refused { reason } => Err(Error::Refused(reason).into()),
        _ => Err(ContractError::Malformed.into()),
    }
}
