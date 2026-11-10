use crate::{
    ContractError, Epoch, HostId, LogicalSessionId, OperationId, PrincipalId, Target,
    CONTROL_FRAME_BYTES, MAX_SESSIONS,
};
use std::sync::{Arc, Mutex};

pub const MAX_CLIENTS: usize = 4;
pub const MAX_PENDING_CREATES: usize = 2;
pub const MAX_CONTROL_OPERATIONS: usize = 8;
pub const MAX_ATTACHMENTS: usize = 4;
pub const MAX_QUEUED_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_BYTES_PER_CLIENT: usize = 512 * 1024;
pub const MAX_IDEMPOTENCY_RECORDS: usize = 128;
pub const MAX_CONTROL_BYTES: usize = MAX_CONTROL_OPERATIONS * CONTROL_FRAME_BYTES;

/// Resource permits reserve capacity only; they convey no access rights.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Resource {
    Client,
    Session,
    PendingCreate,
    Control,
    Attachment,
    TerminalBytes,
    ControlBytes,
    IdempotencyRecord,
}

impl Resource {
    fn slot(self) -> (usize, usize) {
        match self {
            Self::Client => (0, MAX_CLIENTS),
            Self::Session => (1, MAX_SESSIONS),
            Self::PendingCreate => (2, MAX_PENDING_CREATES),
            Self::Control => (3, MAX_CONTROL_OPERATIONS),
            Self::Attachment => (4, MAX_ATTACHMENTS),
            Self::TerminalBytes => (5, MAX_QUEUED_BYTES),
            Self::ControlBytes => (6, MAX_CONTROL_BYTES),
            Self::IdempotencyRecord => (7, MAX_IDEMPOTENCY_RECORDS),
        }
    }
}

#[derive(Clone, Default)]
pub struct Budgets(Arc<Mutex<[usize; 8]>>);

pub struct BudgetPermit {
    state: Arc<Mutex<[usize; 8]>>,
    slot: usize,
    amount: usize,
}

impl Budgets {
    /// Refuse immediately instead of queueing tasks waiting for capacity.
    pub fn reserve(
        &self,
        resource: Resource,
        amount: usize,
    ) -> Result<BudgetPermit, ContractError> {
        let (slot, limit) = resource.slot();
        if amount == 0
            || (!matches!(resource, Resource::TerminalBytes | Resource::ControlBytes)
                && amount != 1)
        {
            return Err(ContractError::Malformed);
        }
        let mut state = self.0.lock().map_err(|_| ContractError::Denied)?;
        let next = state[slot].checked_add(amount).ok_or(ContractError::Busy)?;
        if next > limit {
            return Err(ContractError::Busy);
        }
        state[slot] = next;
        Ok(BudgetPermit {
            state: self.0.clone(),
            slot,
            amount,
        })
    }
}

impl Drop for BudgetPermit {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state[self.slot] -= self.amount;
        }
    }
}

/// Public identity/epoch capture, NOT verified principal proof. Authentication
/// creates the capture locally; never trust a wire copy to admit an operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmissionFence {
    pub host: HostId,
    pub principal: PrincipalId,
    pub connection_epoch: Epoch,
    pub host_epoch: Epoch,
    pub grant_epoch: Epoch,
    pub operation_epoch: Epoch,
    pub operation: OperationId,
    pub logical: Option<LogicalSessionId>,
    pub target: Option<Target>,
}

impl AdmissionFence {
    /// Caller must serialize this check with the protected publication/IPC
    /// effect and revocation. No locks may be held across network I/O.
    pub fn revalidate(&self, current: &Self) -> Result<(), ContractError> {
        if self != current {
            return Err(ContractError::Stale);
        }
        if self
            .target
            .as_ref()
            .is_some_and(|target| self.logical.as_ref() != Some(&target.logical))
        {
            return Err(ContractError::Malformed);
        }
        Ok(())
    }
}
