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
}

impl CreationIntent {
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
        })
    }

    pub(super) fn matches(&self, context: &ApprovedContext) -> bool {
        self.owner == context.approved
            && self.reference == context.reference
            && self.binding == context.binding
    }

    pub fn issued_operation(&self) -> Option<IssuedOperation> {
        self.issued
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|issued| issued.operation().clone())
    }

    /// A correlated control reply was received, not terminal activation.
    pub fn is_resolved(&self) -> bool {
        self.resolved.load(Ordering::Acquire)
    }

    /// Handed to an admitted presentation session; this does not mean Attached.
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
    let issued = {
        let mut original = intent
            .issued
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match original.as_ref() {
            Some(issued) => issued.clone(),
            None => {
                let issued = client.issue_operation()?;
                *original = Some(issued.clone());
                issued
            }
        }
    };
    let request = match intent.mode {
        CreationMode::Create => Request::Create {
            creation: intent.creation.clone(),
        },
        CreationMode::Ensure => Request::Ensure {
            creation: intent.creation.clone(),
        },
    };
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
    intent.resolved.store(true, Ordering::Release);
    drop(flight);
    Ok(Outcome::Created(Box::new(Created {
        intent,
        session,
        disposition,
        attachment,
    })))
}
