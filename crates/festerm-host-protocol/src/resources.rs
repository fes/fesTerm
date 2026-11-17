use crate::{
    ConnectionId, ContractError, Epoch, GenerationId, HostId, LogicalSessionId, OperationId,
    PrincipalId, Request, Target, CONTROL_FRAME_BYTES, MAX_SESSIONS,
};
use std::sync::{Arc, Mutex};

pub const MAX_CLIENTS: usize = 4;
pub const MAX_PENDING_CREATES: usize = 2;
pub const MAX_CONTROL_OPERATIONS: usize = 8;
pub const MAX_ATTACHMENTS: usize = 4;
pub const MAX_QUEUED_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_BYTES_PER_CLIENT: usize = 512 * 1024;
/// Initial recovery transactions, separate from live queued bytes.
pub const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_SNAPSHOTS: usize = 1;
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
    Snapshot,
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
            Self::Snapshot => (8, MAX_SNAPSHOTS),
        }
    }
}

#[derive(Clone, Default)]
pub struct Budgets(Arc<Mutex<[usize; 9]>>);

pub struct BudgetPermit {
    state: Arc<Mutex<[usize; 9]>>,
    slot: usize,
    amount: usize,
}

pub struct ClientBudget {
    budgets: Budgets,
    bytes: Arc<Mutex<usize>>,
    client: Arc<BudgetPermit>,
}

pub struct ClientBytesPermit {
    bytes: Arc<Mutex<usize>>,
    amount: usize,
    _global: BudgetPermit,
    _client: Arc<BudgetPermit>,
}

/// Retains the admitted client and the one shared recovery transaction slot.
/// This is a codec/work bound, not a process RSS measurement or authority.
pub struct SnapshotPermit {
    _snapshot: BudgetPermit,
    _client: Arc<BudgetPermit>,
}

/// Protected local authorization metadata, not client-supplied inventory.
/// Absent means the authoritative journal has reconciled the logical record.
/// Private/revoked/uncertain records must never be treated as an absent grant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogicalState {
    Running(GenerationId),
    /// Ephemeral exact-generation End admission; never a stored lease grant.
    Ended(GenerationId),
    Absent,
    Unknown,
    Revoked,
}

#[derive(Clone, Debug)]
pub struct SessionGrant {
    pub logical: LogicalSessionId,
    pub owner: PrincipalId,
    pub remote_access: bool,
    pub state: LogicalState,
    /// Protected monotonic record revision, advanced on every transition,
    /// including a return to a previous state (Absent -> Running -> Absent).
    pub revision: Epoch,
}

/// Exact request admitted locally; redacted from debug output because recipes
/// and arguments may contain sensitive values.
#[derive(Clone, Eq, PartialEq)]
pub struct AdmittedRequest(Request);

impl From<&Request> for AdmittedRequest {
    fn from(request: &Request) -> Self {
        Self(request.clone())
    }
}

impl std::fmt::Debug for AdmittedRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AdmittedRequest(..)")
    }
}

impl Budgets {
    pub fn admit_client(&self) -> Result<ClientBudget, ContractError> {
        Ok(ClientBudget {
            budgets: self.clone(),
            bytes: Arc::new(Mutex::new(0)),
            client: Arc::new(self.reserve(Resource::Client, 1)?),
        })
    }

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

impl ClientBudget {
    pub fn reserve_snapshot(&self) -> Result<SnapshotPermit, ContractError> {
        Ok(SnapshotPermit {
            _snapshot: self.budgets.reserve(Resource::Snapshot, 1)?,
            _client: self.client.clone(),
        })
    }

    pub fn reserve_terminal_bytes(
        &self,
        amount: usize,
    ) -> Result<ClientBytesPermit, ContractError> {
        let mut bytes = self.bytes.lock().map_err(|_| ContractError::Denied)?;
        let next = bytes.checked_add(amount).ok_or(ContractError::Busy)?;
        if next > MAX_BYTES_PER_CLIENT {
            return Err(ContractError::Busy);
        }
        let global = self.budgets.reserve(Resource::TerminalBytes, amount)?;
        *bytes = next;
        Ok(ClientBytesPermit {
            bytes: self.bytes.clone(),
            amount,
            _global: global,
            _client: self.client.clone(),
        })
    }
}

impl Drop for ClientBytesPermit {
    fn drop(&mut self) {
        if let Ok(mut bytes) = self.bytes.lock() {
            *bytes -= self.amount;
        }
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
    pub connection: ConnectionId,
    pub connection_epoch: Epoch,
    pub host_epoch: Epoch,
    pub grant_epoch: Epoch,
    pub operation_epoch: Epoch,
    pub operation: OperationId,
    pub intent: AdmittedRequest,
    pub issuance_epoch: Option<Epoch>,
    pub logical: Option<LogicalSessionId>,
    pub logical_state: Option<LogicalState>,
    pub logical_revision: Option<Epoch>,
    pub target: Option<Target>,
}

impl AdmissionFence {
    /// Caller must serialize this check with the protected publication/IPC
    /// effect and revocation. No locks may be held across network I/O.
    pub fn revalidate(&self, current: &Self) -> Result<(), ContractError> {
        if self != current {
            return Err(ContractError::Stale);
        }
        let observed_end = matches!(
            (&self.logical_state, &self.intent.0, &self.target),
            (Some(LogicalState::Ended(generation)), Request::ExplicitEnd { target: requested }, Some(target))
                if requested == target && *generation == target.generation
        );
        if self.logical.is_some() != self.logical_state.is_some()
            || self.logical.is_some() != self.logical_revision.is_some()
            || matches!(
                self.logical_state,
                Some(LogicalState::Unknown | LogicalState::Revoked)
            )
            || matches!(self.logical_state, Some(LogicalState::Ended(_))) && !observed_end
        {
            return Err(ContractError::Malformed);
        }
        if self.target.as_ref().is_some_and(|target| {
            self.logical.as_ref() != Some(&target.logical)
                || self.logical_state != Some(LogicalState::Running(target.generation))
                    && !observed_end
        }) {
            return Err(ContractError::Malformed);
        }
        Ok(())
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    #[test]
    fn snapshot_slot_is_shared_and_independent_of_live_and_control_capacity() {
        let budgets = Budgets::default();
        let first = budgets.admit_client().unwrap();
        let second = budgets.clone().admit_client().unwrap();
        let snapshot = first.reserve_snapshot().unwrap();
        assert!(matches!(
            second.reserve_snapshot(),
            Err(ContractError::Busy)
        ));
        let live = first.reserve_terminal_bytes(MAX_BYTES_PER_CLIENT).unwrap();
        assert!(matches!(
            first.reserve_terminal_bytes(1),
            Err(ContractError::Busy)
        ));
        let control = budgets
            .reserve(Resource::ControlBytes, MAX_CONTROL_BYTES)
            .unwrap();
        drop(snapshot);
        assert!(second.reserve_snapshot().is_ok());
        drop((live, control));
    }
    #[test]
    fn snapshot_retains_client_admission_until_last_work_owner_drops() {
        let budgets = Budgets::default();
        let first = budgets.admit_client().unwrap();
        let snapshot = Arc::new(first.reserve_snapshot().unwrap());
        let late_reader = snapshot.clone();
        drop(first);
        let others: Vec<_> = (1..MAX_CLIENTS)
            .map(|_| budgets.admit_client().unwrap())
            .collect();
        drop(snapshot);
        assert!(matches!(budgets.admit_client(), Err(ContractError::Busy)));
        assert!(matches!(
            others[0].reserve_snapshot(),
            Err(ContractError::Busy)
        ));
        drop(late_reader);
        assert!(budgets.admit_client().is_ok());
        assert!(others[0].reserve_snapshot().is_ok());
    }
}
