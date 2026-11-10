use crate::{
    ContractError, Cursor, Epoch, HostId, OperationId, PrincipalId, SessionSummary, Target,
    Validate, MAX_SESSIONS, PAGE_SIZE,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryPage {
    pub snapshot: OperationId,
    pub authorization_epoch: Epoch,
    pub sessions: Vec<SessionSummary>,
    /// Repeated across pages: these admitted targets could not be observed.
    /// Neither omission nor an incomplete inventory establishes absence.
    pub unknown: Vec<Target>,
    pub next: Option<Cursor>,
}

impl Validate for InventoryPage {
    fn validate(&self) -> Result<(), ContractError> {
        if self.sessions.len() > PAGE_SIZE || self.unknown.len() > MAX_SESSIONS {
            return Err(ContractError::Limit);
        }
        let mut previous = None;
        for session in &self.sessions {
            session.validate()?;
            if previous.is_some_and(|id| id >= &session.target.logical) {
                return Err(ContractError::Malformed);
            }
            previous = Some(&session.target.logical);
        }
        let mut previous = None;
        for target in &self.unknown {
            if previous.is_some_and(|id| id >= &target.logical)
                || self
                    .sessions
                    .iter()
                    .any(|session| session.target.logical == target.logical)
            {
                return Err(ContractError::Malformed);
            }
            previous = Some(&target.logical);
        }
        if let Some(cursor) = &self.next {
            cursor.validate()?;
            if cursor.snapshot != self.snapshot || self.sessions.len() != PAGE_SIZE {
                return Err(ContractError::Malformed);
            }
        }
        Ok(())
    }
}

/// Private local metadata. Never serialize this source table to a remote peer.
pub struct InventoryEntry {
    pub owner: PrincipalId,
    pub shared: bool,
    pub revoked: bool,
    pub session: SessionSummary,
}

pub struct UnknownInventoryEntry {
    pub owner: PrincipalId,
    pub shared: bool,
    pub revoked: bool,
    pub target: Target,
}

/// In-memory snapshot bound to already authenticated owner and current grants.
/// Construction is filtering, not authentication. Retain at most one snapshot
/// per admitted client; retire on disconnect/deadline or any epoch transition.
pub struct AuthorizedInventory {
    host: HostId,
    principal: PrincipalId,
    epoch: Epoch,
    snapshot: OperationId,
    sessions: Vec<SessionSummary>,
    unknown: Vec<Target>,
}

impl AuthorizedInventory {
    pub fn capture(
        host: HostId,
        principal: PrincipalId,
        epoch: Epoch,
        snapshot: OperationId,
        entries: impl IntoIterator<Item = InventoryEntry>,
    ) -> Result<Self, ContractError> {
        Self::capture_with_unknowns(host, principal, epoch, snapshot, entries, [])
    }

    pub fn capture_with_unknowns(
        host: HostId,
        principal: PrincipalId,
        epoch: Epoch,
        snapshot: OperationId,
        entries: impl IntoIterator<Item = InventoryEntry>,
        uncertain: impl IntoIterator<Item = UnknownInventoryEntry>,
    ) -> Result<Self, ContractError> {
        let mut sessions = Vec::new();
        for (index, entry) in entries.into_iter().enumerate() {
            if index >= MAX_SESSIONS {
                return Err(ContractError::Limit);
            }
            if entry.owner == principal && entry.shared && !entry.revoked {
                entry.session.validate()?;
                sessions.push(entry.session);
            }
        }
        sessions.sort_by(|a, b| a.target.logical.cmp(&b.target.logical));
        if sessions
            .windows(2)
            .any(|pair| pair[0].target.logical == pair[1].target.logical)
        {
            return Err(ContractError::Malformed);
        }
        let mut unknown = Vec::new();
        for (index, entry) in uncertain.into_iter().enumerate() {
            if index >= MAX_SESSIONS {
                return Err(ContractError::Limit);
            }
            if entry.owner == principal && entry.shared && !entry.revoked {
                unknown.push(entry.target);
            }
        }
        unknown.sort_by(|a, b| a.logical.cmp(&b.logical));
        if sessions.len() + unknown.len() > MAX_SESSIONS {
            return Err(ContractError::Limit);
        }
        if unknown
            .windows(2)
            .any(|pair| pair[0].logical == pair[1].logical)
            || unknown.iter().any(|target| {
                sessions
                    .iter()
                    .any(|session| session.target.logical == target.logical)
            })
        {
            return Err(ContractError::Malformed);
        }
        Ok(Self {
            host,
            principal,
            epoch,
            snapshot,
            sessions,
            unknown,
        })
    }

    pub fn page(
        &self,
        host: &HostId,
        principal: &PrincipalId,
        epoch: Epoch,
        cursor: Option<&Cursor>,
    ) -> Result<InventoryPage, ContractError> {
        if host != &self.host || principal != &self.principal || epoch != self.epoch {
            return Err(ContractError::Stale);
        }
        let offset = if let Some(cursor) = cursor {
            cursor.validate()?;
            if cursor.snapshot != self.snapshot {
                return Err(ContractError::Stale);
            }
            usize::from(cursor.offset)
        } else {
            0
        };
        if offset > self.sessions.len() || (offset != 0 && offset == self.sessions.len()) {
            return Err(ContractError::Stale);
        }
        let end = (offset + PAGE_SIZE).min(self.sessions.len());
        Ok(InventoryPage {
            snapshot: self.snapshot.clone(),
            authorization_epoch: self.epoch,
            sessions: self.sessions[offset..end].to_vec(),
            unknown: self.unknown.clone(),
            next: (end < self.sessions.len()).then(|| Cursor {
                snapshot: self.snapshot.clone(),
                offset: end as u16,
            }),
        })
    }
}
