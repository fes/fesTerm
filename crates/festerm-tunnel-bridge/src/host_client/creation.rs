use super::*;
use festerm_host_protocol::{Creation, EnsureDisposition, IssuedOperation, Lifetime, Refusal};
use festerm_tunnel_client::host_control::IssuedControlOperation;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CreationMode {
    Create,
    Ensure,
}

/// One explicit action. Neither reconnect nor an uncertain reply may replace
/// its logical identity, future attachment, recipe or captured issuer.
pub struct CreationIntent {
    owner: OwnerBinding,
    reference: SecretReference,
    binding: Binding,
    mode: CreationMode,
    creation: Creation,
    issued: Mutex<Option<IssuedControlOperation>>,
    active: AtomicBool,
    resolved: AtomicBool,
    presented: AtomicBool,
    original: Mutex<Option<Arc<originals::OriginalHandle>>>,
    restored: Option<IssuedOperation>,
    confirmed: Mutex<Option<Target>>,
}

impl CreationIntent {
    pub fn is_protected_original(&self) -> bool {
        self.original
            .lock()
            .map(|original| original.is_some())
            .unwrap_or(true)
    }

    pub fn new(
        context: &ApprovedContext,
        mode: CreationMode,
        creation: Creation,
    ) -> Result<Self, Failure> {
        creation.validate()?;
        Ok(Self {
            owner: context.approved.clone(),
            reference: context.reference.duplicate_for_transport(),
            binding: context.binding.clone(),
            mode,
            creation,
            issued: Mutex::new(None),
            active: AtomicBool::new(false),
            resolved: AtomicBool::new(false),
            presented: AtomicBool::new(false),
            original: Mutex::new(None),
            restored: None,
            confirmed: Mutex::new(None),
        })
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
            if self.issued_operation().is_some() {
                return Err(Failure::OriginalStorage);
            }
            *original = Some(store.handle(self.creation.attachment.to_string(), review));
        }
        Ok(())
    }

    pub(super) fn from_original(
        context: &ApprovedContext,
        mode: CreationMode,
        creation: Creation,
        handle: Arc<originals::OriginalHandle>,
        issued: IssuedOperation,
        confirmed: Option<Target>,
    ) -> Result<Self, Failure> {
        let mut intent = Self::new(context, mode, creation)?;
        intent.original = Mutex::new(Some(handle));
        intent.restored = Some(issued);
        intent.confirmed = Mutex::new(confirmed);
        Ok(intent)
    }

    pub(super) fn matches(&self, context: &ApprovedContext) -> bool {
        self.owner == context.approved
            && self.reference == context.reference
            && self.binding == context.binding
    }

    pub fn matches_context(&self, context: &ApprovedContext) -> bool {
        self.matches(context)
    }

    pub fn confirmed_target(&self) -> Option<Target> {
        self.confirmed.lock().ok().and_then(|target| target.clone())
    }

    pub(super) fn settled_by(
        &self,
        owner: &OwnerBinding,
        key: &SecretReference,
        binding: &Binding,
        target: &Target,
    ) -> bool {
        self.owner == *owner
            && self.reference == *key
            && self.binding == *binding
            && self.confirmed_target().as_ref() == Some(target)
    }

    pub fn mode(&self) -> CreationMode {
        self.mode
    }

    pub fn issued_operation(&self) -> Option<IssuedOperation> {
        self.issued
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|issued| issued.operation().clone())
            .or_else(|| self.restored.clone())
    }

    /// A correlated control reply was received, not terminal activation.
    pub fn is_resolved(&self) -> bool {
        self.resolved.load(Ordering::Acquire)
    }

    /// The originating frontend adopted recovery and the host confirmed activation.
    pub fn is_presented(&self) -> bool {
        self.presented.load(Ordering::Acquire)
    }

    pub(super) fn mark_presented(&self) {
        self.presented.store(true, Ordering::Release);
    }

    pub fn creation(&self) -> &Creation {
        &self.creation
    }
}

pub struct Created {
    pub(super) intent: Arc<CreationIntent>,
    pub(super) session: SessionSummary,
    disposition: EnsureDisposition,
    pub(super) attachment: AttachmentId,
}

impl Created {
    pub fn session(&self) -> &SessionSummary {
        &self.session
    }

    pub fn disposition(&self) -> EnsureDisposition {
        self.disposition
    }
}

struct Flight<'a>(&'a AtomicBool);

impl Drop for Flight<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(crate) async fn create_authenticated<S: AsyncRead + AsyncWrite + Unpin>(
    client: &mut ControlClient<S>,
    intent: Arc<CreationIntent>,
    cancel: &CancellationToken,
) -> Result<Outcome, Failure> {
    if client.binding() != &intent.binding {
        return Err(Failure::BindingMismatch);
    }
    if intent.active.swap(true, Ordering::AcqRel) {
        return Err(ContractError::Busy.into());
    }
    let flight = Flight(&intent.active);
    if cancel.is_cancelled() {
        return Err(Error::Cancelled.into());
    }
    let capabilities = client.handshake(cancel).await?;
    let supported = match intent.mode {
        CreationMode::Create => capabilities.create,
        CreationMode::Ensure => capabilities.ensure,
    };
    if !supported || !capabilities.attach {
        return Err(Error::Refused(Refusal::Unsupported).into());
    }
    if intent.mode == CreationMode::Create && !capabilities.creation_allowed {
        return Err(Error::Refused(Refusal::Denied).into());
    }
    if matches!(intent.creation.lifetime, Lifetime::Temporary { .. }) && !capabilities.temporary {
        return Err(Error::Refused(Refusal::Unsupported).into());
    }
    let request = match intent.mode {
        CreationMode::Create => Request::Create {
            creation: intent.creation.clone(),
        },
        CreationMode::Ensure => Request::Ensure {
            creation: intent.creation.clone(),
        },
    };
    let protected = intent
        .original
        .lock()
        .map_err(|_| Failure::OriginalStorage)?
        .clone();
    let issued = {
        let mut original = intent
            .issued
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match original.as_ref() {
            Some(issued) => issued.clone(),
            None => {
                let issued = if intent.restored.is_some() {
                    let handle = protected.as_ref().ok_or(Failure::OriginalStorage)?;
                    client.reopen_protected_original(&request, handle.as_ref())?
                } else {
                    client.issue_operation()?
                };
                *original = Some(issued.clone());
                issued
            }
        }
    };
    if let Some(protected) = &protected {
        protected.capture_creation(intent.mode, &intent.creation, &issued, &request)?;
    }
    let response = client.request_issued(&issued, request, cancel).await?;
    let Response::Ensured {
        session,
        disposition,
        lease,
    } = response.body
    else {
        return Err(match response.body {
            Response::Refused { reason } => Error::Refused(reason).into(),
            _ => Failure::from(ContractError::Malformed),
        });
    };
    let attachment = lease
        .map(|lease| lease.attachment)
        .unwrap_or_else(|| intent.creation.attachment.clone());
    if let Some(protected) = &protected {
        protected.confirmed_creation(&session.target)?;
    }
    *intent
        .confirmed
        .lock()
        .map_err(|_| Failure::OriginalStorage)? = Some(session.target.clone());
    intent.resolved.store(true, Ordering::Release);
    drop(flight);
    Ok(Outcome::Created(Box::new(Created {
        intent,
        session,
        disposition,
        attachment,
    })))
}
