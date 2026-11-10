//! Local-only durable HOST session management. `admit` freshly authorizes current
//! protected state and returns a scoped policy guard. Guards cover durable effect
//! commits/publication, but are released before waiting for native launch/IPC.
//! This module neither authenticates a remote caller nor installs an endpoint.
use crate::{
    detached,
    host_lease::{now_ms, DaemonLease, LeaseGuard, PendingAttachment as PendingLeaseAttachment},
    private_storage as storage, NativeSessionIdentity, PersistentSession,
};
use festerm_host_protocol::{
    AttachmentId, Creation, DetachKind, Epoch, GenerationId, LeaseId, Lifetime, LogicalSessionId,
    LogicalState, OperationId, PrincipalId, Recipe, SessionGrant, SessionSummary, Target,
    TemporaryLease, Validate, MAX_IDEMPOTENCY_RECORDS, MAX_PENDING_CREATES, MAX_SESSIONS,
};
use festerm_pty::LocalProfile;
use festerm_session::{SessionEventNotifier, TerminalSize};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fmt,
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

const STATE_LIMIT: usize = 4 * 1024 * 1024;
const STATE_FILE: &str = "host-sessions.json";
const HISTORY_LIMIT: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlError {
    Denied,
    Revoked,
    Unknown,
    Busy,
    Stale,
    RecipeConflict,
    OperationConflict,
    AlreadyExists,
    Ended,
    Cancelled,
    Invalid,
    Storage,
    /// A process may exist. Never report cancellation/absence or retry a spawn.
    CommittedButNotPublished,
}

impl fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "host session control: {self:?}")
    }
}
impl std::error::Error for ControlError {}

fn storage_error(error: io::Error) -> ControlError {
    if crate::lock_is_contended(&error) {
        ControlError::Busy
    } else {
        ControlError::Storage
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    Lookup,
    Resume,
    Create,
    Attach,
    End,
    Lease,
}

/// Built only from private reconciled state, never from wire inventory.
pub struct AdmissionState {
    pub grant: Option<SessionGrant>,
    pub grant_epoch: Option<Epoch>,
    pub effect: Effect,
}

#[derive(Clone, Debug)]
pub struct EnsureResult {
    pub summary: SessionSummary,
    pub target: Target,
    pub identity: NativeSessionIdentity,
    pub lifetime: Lifetime,
    pub lease: Option<TemporaryLease>,
    pub created: bool,
    pub recreated: bool,
    /// Cancellation cannot undo committed creation or disguise its outcome.
    pub cancelled: bool,
}

/// Admission has started, but recovery adoption and lease transfer have NOT
/// committed. Do not send an Attached success until `attachment_outcome` passes.
pub struct PendingAttachment {
    pub session: PersistentSession,
    pub ticket: Option<OperationId>,
    pub reserved_lease: Option<TemporaryLease>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttachmentCancellation {
    CancelledBeforeCommit,
    Committed(TemporaryLease),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndOutcome {
    StopRequested,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Lookup {
    Absent,
    Running(Target),
    Unknown,
    Revoked,
}

pub struct ProtectedSession {
    pub grant: SessionGrant,
    pub summary: SessionSummary,
}

pub struct ProtectedLookup {
    pub state: Lookup,
    pub grant: Option<SessionGrant>,
    pub summary: Option<SessionSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    pid: u32,
    endpoint: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Phase {
    Launching,
    Running,
    Absent,
    Ending,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    owner: PrincipalId,
    logical: LogicalSessionId,
    display_name: String,
    recipe: Recipe,
    effective_recipe: Recipe,
    lifetime: Lifetime,
    epoch: Epoch,
    revoked: bool,
    name: String,
    launch: OperationId,
    generation: GenerationId,
    fingerprint: String,
    phase: Phase,
    identity: Option<Identity>,
}

impl Record {
    fn grant(&self) -> SessionGrant {
        SessionGrant {
            logical: self.logical.clone(),
            owner: self.owner.clone(),
            revision: self.epoch,
            remote_access: !self.revoked,
            state: self.state(),
        }
    }

    fn native(&self) -> Option<NativeSessionIdentity> {
        self.identity
            .as_ref()
            .map(|identity| NativeSessionIdentity {
                name: self.name.clone(),
                pid: identity.pid,
                created_at_unix_ms: self.generation.get(),
                endpoint: identity.endpoint.clone(),
            })
    }
    fn target(&self) -> Target {
        Target {
            logical: self.logical.clone(),
            generation: self.generation,
        }
    }
    fn state(&self) -> LogicalState {
        if self.revoked {
            return LogicalState::Revoked;
        }
        match self.phase {
            Phase::Running => LogicalState::Running(self.generation),
            Phase::Absent => LogicalState::Absent,
            Phase::Launching | Phase::Ending => LogicalState::Unknown,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    owner: PrincipalId,
    creation: Creation,
    generation: GenerationId,
    created: bool,
    recreated: bool,
    create_only: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationReceipt {
    owner: PrincipalId,
    intent: String,
    generation: GenerationId,
    created: bool,
    recreated: bool,
}

impl OperationReceipt {
    fn intent(
        owner: &PrincipalId,
        creation: &Creation,
        create_only: bool,
    ) -> Result<String, ControlError> {
        let bytes = serde_json::to_vec(&(owner, creation, create_only))
            .map_err(|_| ControlError::Storage)?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }

    fn from_operation(operation: &Operation) -> Result<Self, ControlError> {
        Ok(Self {
            owner: operation.owner.clone(),
            intent: Self::intent(&operation.owner, &operation.creation, operation.create_only)?,
            generation: operation.generation,
            created: operation.created,
            recreated: operation.recreated,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetiredSession {
    owner: PrincipalId,
    logical: LogicalSessionId,
    generation: GenerationId,
    epoch: Epoch,
    revoked: bool,
    uncertain: bool,
}

impl RetiredSession {
    fn grant(&self) -> SessionGrant {
        SessionGrant {
            logical: self.logical.clone(),
            owner: self.owner.clone(),
            revision: self.epoch,
            remote_access: !self.revoked,
            state: if self.revoked {
                LogicalState::Revoked
            } else if self.uncertain {
                LogicalState::Unknown
            } else {
                LogicalState::Absent
            },
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u8,
    sessions: BTreeMap<LogicalSessionId, Record>,
    operations: BTreeMap<OperationId, Operation>,
    #[serde(default)]
    last_generation: u128,
}

struct Transaction {
    directory: File,
    _lock: File,
    state: State,
}

impl Transaction {
    fn history<T: serde::de::DeserializeOwned>(
        &self,
        name: &Path,
    ) -> Result<Option<T>, ControlError> {
        match storage::read_private(&self.directory, name, HISTORY_LIMIT) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| ControlError::Storage),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(storage_error(error)),
        }
    }

    fn write_history(&self, name: &Path, value: &impl Serialize) -> Result<(), ControlError> {
        let bytes = serde_json::to_vec(value).map_err(|_| ControlError::Storage)?;
        if bytes.len() > HISTORY_LIMIT {
            return Err(ControlError::Storage);
        }
        // The global journal lock owns publication. Atomic replacement of a
        // missing entry avoids exposing a partially written permanent fence.
        match storage::read_private(&self.directory, name, HISTORY_LIMIT) {
            Ok(old) => {
                if old.as_slice() == bytes.as_slice() {
                    Ok(())
                } else {
                    Err(ControlError::Storage)
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                storage::replace_private(&self.directory, name, &bytes).map_err(storage_error)
            }
            Err(error) => Err(storage_error(error)),
        }
    }

    fn receipt(&self, operation: &OperationId) -> Result<Option<OperationReceipt>, ControlError> {
        self.history(Path::new(&format!("host-receipt-{operation}.json")))
    }

    fn retired(&self, logical: &LogicalSessionId) -> Result<Option<RetiredSession>, ControlError> {
        let retired: Option<RetiredSession> =
            self.history(Path::new(&format!("host-retired-{logical}.json")))?;
        if retired
            .as_ref()
            .is_some_and(|record| &record.logical != logical)
        {
            return Err(ControlError::Storage);
        }
        Ok(retired)
    }

    fn retire_operations(
        &mut self,
        logical: Option<&LogicalSessionId>,
    ) -> Result<(), ControlError> {
        let completed: Vec<_> = self
            .state
            .operations
            .iter()
            .filter(|(_, operation)| {
                logical.is_some_and(|logical| logical == &operation.creation.logical)
                    || self
                        .state
                        .sessions
                        .get(&operation.creation.logical)
                        .is_some_and(|record| !matches!(record.phase, Phase::Launching))
            })
            .map(|(id, _)| id.clone())
            .collect();
        if completed.is_empty() {
            return Ok(());
        }
        for id in completed {
            let receipt = OperationReceipt::from_operation(&self.state.operations[&id])?;
            self.write_history(Path::new(&format!("host-receipt-{id}.json")), &receipt)?;
            self.state.operations.remove(&id);
        }
        self.save()
    }

    fn retire_record(&mut self, logical: &LogicalSessionId) -> Result<(), ControlError> {
        let record = self
            .state
            .sessions
            .get(logical)
            .ok_or(ControlError::Denied)?;
        let retired = RetiredSession {
            owner: record.owner.clone(),
            logical: logical.clone(),
            generation: record.generation,
            epoch: record.epoch.next().map_err(|_| ControlError::Storage)?,
            revoked: record.revoked,
            uncertain: !matches!(record.phase, Phase::Absent),
        };
        // Receipt/tombstone durability precedes removal from the active journal.
        // A crash between these writes leaves a sealed, not reusable identity.
        self.write_history(Path::new(&format!("host-retired-{logical}.json")), &retired)?;
        self.retire_operations(Some(logical))?;
        self.state.last_generation = self.state.last_generation.max(retired.generation.get());
        self.state.sessions.remove(logical);
        self.save()
    }

    fn save(&self) -> Result<(), ControlError> {
        let bytes = serde_json::to_vec(&self.state).map_err(|_| ControlError::Storage)?;
        if bytes.len() > STATE_LIMIT {
            return Err(ControlError::Busy);
        }
        storage::replace_private(&self.directory, Path::new(STATE_FILE), &bytes)
            .map_err(storage_error)
    }
}

/// The helper path is install-owned local configuration, never a request field.
/// One private journal lock serializes all owners/logicals (stronger than the
/// required per-owner key); contention refuses without queued threads/tasks.
pub struct SessionControl {
    root: PathBuf,
    helper: PathBuf,
}

impl SessionControl {
    pub fn open(root: &Path) -> Result<Self, ControlError> {
        let helper =
            crate::daemon_executable_for_root(Some(root)).map_err(|_| ControlError::Storage)?;
        Self::with_helper(root, &helper)
    }

    /// Explicit install/test seam. The caller must verify the installed helper
    /// exactly as for ordinary native launches. Never accept this path remotely.
    pub fn with_helper(root: &Path, helper: &Path) -> Result<Self, ControlError> {
        if !root.is_absolute() || !helper.is_absolute() || !helper.is_file() {
            return Err(ControlError::Invalid);
        }
        detached::prepare_native_runtime(root).map_err(|_| ControlError::Storage)?;
        Ok(Self {
            root: root.into(),
            helper: helper.into(),
        })
    }

    fn transaction(&self) -> Result<Transaction, ControlError> {
        let directory = storage::private_directory(&self.root, false).map_err(storage_error)?;
        let lock =
            match storage::open_private_file(&directory, Path::new("host-sessions.lock"), true) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    storage::open_private_file(&directory, Path::new("host-sessions.lock"), false)
                        .map_err(storage_error)?
                }
                Err(error) => return Err(storage_error(error)),
            };
        FileExt::try_lock_exclusive(&lock).map_err(|error| {
            if crate::lock_is_contended(&error) {
                ControlError::Busy
            } else {
                ControlError::Storage
            }
        })?;
        let state = match storage::read_private(&directory, Path::new(STATE_FILE), STATE_LIMIT) {
            Ok(bytes) => {
                serde_json::from_slice::<State>(&bytes).map_err(|_| ControlError::Storage)?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => State {
                version: 1,
                ..State::default()
            },
            Err(error) => return Err(storage_error(error)),
        };
        if state.version != 1
            || state.sessions.len() > MAX_SESSIONS
            || state.operations.len() > MAX_IDEMPOTENCY_RECORDS
        {
            return Err(ControlError::Storage);
        }
        for (logical, record) in &state.sessions {
            if logical != &record.logical
                || record.name != native_name(&record.owner, logical)
                || record.recipe.validate().is_err()
                || record.effective_recipe.validate().is_err()
                || record.lifetime.validate().is_err()
                || matches!(record.phase, Phase::Running) && record.identity.is_none()
            {
                return Err(ControlError::Storage);
            }
        }
        for operation in state.operations.values() {
            operation
                .creation
                .validate()
                .map_err(|_| ControlError::Storage)?;
        }
        let mut tx = Transaction {
            directory,
            _lock: lock,
            state,
        };
        // Complete an interrupted retirement before any native work.
        let logicals: Vec<_> = tx.state.sessions.keys().cloned().collect();
        for logical in logicals {
            if let Some(retired) = tx.retired(&logical)? {
                let record = &tx.state.sessions[&logical];
                if retired.owner != record.owner
                    || retired.generation != record.generation
                    || retired.epoch != record.epoch.next().map_err(|_| ControlError::Storage)?
                {
                    return Err(ControlError::Storage);
                }
                tx.retire_record(&logical)?;
            }
        }
        tx.retire_operations(None)?;
        Ok(tx)
    }

    fn admission<G>(
        record: Option<&Record>,
        owner: &PrincipalId,
        effect: Effect,
        admit: &mut impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<G, ControlError> {
        if record.is_some_and(|record| &record.owner != owner) {
            return Err(ControlError::Denied);
        }
        let state = AdmissionState {
            grant: record.map(Record::grant),
            grant_epoch: record.map(|record| record.epoch),
            effect,
        };
        let guard = admit(&state)?;
        if record.is_some_and(|record| record.revoked) {
            return Err(ControlError::Revoked);
        }
        Ok(guard)
    }

    fn admit_retired<G>(
        retired: &RetiredSession,
        owner: &PrincipalId,
        effect: Effect,
        admit: &mut impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<G, ControlError> {
        if &retired.owner != owner {
            return Err(ControlError::Denied);
        }
        let guard = admit(&AdmissionState {
            grant: Some(retired.grant()),
            grant_epoch: Some(retired.epoch),
            effect,
        })?;
        if retired.revoked {
            return Err(ControlError::Revoked);
        }
        Ok(guard)
    }

    fn reconcile(&self, record: &mut Record) -> Result<(), ControlError> {
        if record.revoked || matches!(record.phase, Phase::Absent) {
            return Ok(());
        }
        let previous = record.phase.clone();
        // A pending reservation can only adopt its exact journaled incarnation.
        // It must never enter the spawning path again after restart/lost reply.
        if matches!(record.phase, Phase::Launching) {
            let profile = profile(&record.effective_recipe)?;
            let request = detached::DetachedSessionRequest {
                name: &record.name,
                profile: &profile,
                size: terminal_size(),
                scrollback_limit_bytes: 4096,
                launch_request: record.launch.as_str(),
            };
            if let Ok(identity) = detached::ensure_host_reserved(
                &self.root,
                request,
                record.generation.get(),
                false,
                &self.helper,
            ) {
                record.identity = Some(Identity {
                    pid: identity.pid,
                    endpoint: identity.endpoint,
                });
                record.phase = Phase::Running;
            }
        }
        if let Some(identity) = record.native() {
            match detached::host_generation_state(&self.root, &identity, &record.fingerprint) {
                Ok(true) => {}
                Ok(false) => record.phase = Phase::Absent,
                // Observation failure is not a transition back to creation.
                // Keep the bound incarnation and phase for a later retry.
                Err(_) => return Err(ControlError::Unknown),
            }
        }
        if record.phase != previous {
            record.epoch = record.epoch.next().map_err(|_| ControlError::Storage)?;
        }
        Ok(())
    }

    pub fn lookup<G>(
        &self,
        owner: &PrincipalId,
        logical: &LogicalSessionId,
        admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<Lookup, ControlError> {
        Ok(self.inspect(owner, logical, admit)?.state)
    }

    pub fn inspect<G>(
        &self,
        owner: &PrincipalId,
        logical: &LogicalSessionId,
        mut admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<ProtectedLookup, ControlError> {
        let mut tx = self.transaction()?;
        if let Some(retired) = tx.retired(logical)? {
            let _publication = Self::admit_retired(&retired, owner, Effect::Lookup, &mut admit)?;
            return Ok(ProtectedLookup {
                state: if retired.uncertain {
                    Lookup::Unknown
                } else {
                    Lookup::Absent
                },
                grant: Some(retired.grant()),
                summary: None,
            });
        }
        if let Some(record) = tx.state.sessions.get_mut(logical) {
            if &record.owner != owner {
                return Err(ControlError::Denied);
            }
            self.reconcile(record)?;
        }
        tx.save()?;
        let record = tx.state.sessions.get(logical);
        let _publication = Self::admission(record, owner, Effect::Lookup, &mut admit)?;
        let state = match record.map(Record::state) {
            None | Some(LogicalState::Absent) => Lookup::Absent,
            Some(LogicalState::Running(generation)) => Lookup::Running(Target {
                logical: logical.clone(),
                generation,
            }),
            Some(LogicalState::Revoked) => Lookup::Revoked,
            Some(LogicalState::Unknown) => Lookup::Unknown,
        };
        let summary = match record {
            Some(record) if matches!(record.phase, Phase::Running) => Some(self.summary(record)?),
            _ => None,
        };
        Ok(ProtectedLookup {
            state,
            grant: record.map(Record::grant),
            summary,
        })
    }

    /// Bounded protected inventory. Filtering precedes label generation; no
    /// frontend lease or takeover is acquired to inspect native metadata.
    pub fn inventory<G>(
        &self,
        owner: &PrincipalId,
        mut admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<Vec<ProtectedSession>, ControlError> {
        let mut tx = self.transaction()?;
        Self::admission(None, owner, Effect::Lookup, &mut admit)?;
        for record in tx
            .state
            .sessions
            .values_mut()
            .filter(|record| &record.owner == owner && !record.revoked)
        {
            self.reconcile(record)?;
        }
        tx.save()?;
        let mut result = Vec::new();
        for record in tx.state.sessions.values().filter(|record| {
            &record.owner == owner && !record.revoked && matches!(record.phase, Phase::Running)
        }) {
            let _admission = Self::admission(Some(record), owner, Effect::Lookup, &mut admit)?;
            result.push(ProtectedSession {
                grant: record.grant(),
                summary: self.summary(record)?,
            });
        }
        let _publication = Self::admission(None, owner, Effect::Lookup, &mut admit)?;
        Ok(result)
    }

    /// Explicit Connect/repair only, not a background reconnect primitive.
    /// `create_only` refuses an already existing logical identity, even if dead.
    pub fn ensure<G>(
        &self,
        owner: &PrincipalId,
        operation: &OperationId,
        creation: &Creation,
        create_only: bool,
        cancelled: &AtomicBool,
        mut admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<EnsureResult, ControlError> {
        creation.validate().map_err(|_| ControlError::Invalid)?;
        let mut tx = self.transaction()?;
        let logical = &creation.logical;
        if let Some(retired) = tx.retired(logical)? {
            let _admission = Self::admit_retired(&retired, owner, Effect::Resume, &mut admit)?;
            return Err(if retired.uncertain {
                ControlError::Unknown
            } else {
                ControlError::Ended
            });
        }
        if let Some(record) = tx.state.sessions.get_mut(logical) {
            if &record.owner != owner {
                return Err(ControlError::Denied);
            }
            self.reconcile(record)?;
            tx.save()?;
        }
        Self::admission(
            tx.state.sessions.get(logical),
            owner,
            Effect::Resume,
            &mut admit,
        )?;
        if let Some(receipt) = tx.receipt(operation)? {
            if receipt.owner != *owner
                || receipt.intent != OperationReceipt::intent(owner, creation, create_only)?
            {
                return Err(ControlError::OperationConflict);
            }
            let record = tx.state.sessions.get(logical).ok_or(ControlError::Ended)?;
            if record.generation != receipt.generation {
                return Err(ControlError::Ended);
            }
            let _publication = Self::admission(Some(record), owner, Effect::Resume, &mut admit)?;
            return self.result(record, receipt.created, receipt.recreated, cancelled);
        }
        if let Some(previous) = tx.state.operations.get(operation) {
            if &previous.owner != owner
                || previous.creation != *creation
                || previous.create_only != create_only
            {
                return Err(ControlError::OperationConflict);
            }
            let record = tx
                .state
                .sessions
                .get_mut(logical)
                .ok_or(ControlError::Storage)?;
            if record.generation != previous.generation {
                return Err(ControlError::Ended);
            }
            self.reconcile(record)?;
            tx.save()?;
            let record = &tx.state.sessions[logical];
            let _publication = Self::admission(Some(record), owner, Effect::Resume, &mut admit)?;
            return self.result(record, previous.created, previous.recreated, cancelled);
        }
        if cancelled.load(Ordering::Acquire) {
            return Err(ControlError::Cancelled);
        }
        if tx.state.operations.len() >= MAX_IDEMPOTENCY_RECORDS {
            return Err(ControlError::Busy);
        }
        if let Some(record) = tx.state.sessions.get_mut(logical) {
            if create_only {
                return Err(ControlError::AlreadyExists);
            }
            self.reconcile(record)?;
            if !matches!(record.phase, Phase::Absent)
                && (record.recipe != creation.recipe || record.lifetime != creation.lifetime)
            {
                return Err(ControlError::RecipeConflict);
            }
            if matches!(record.phase, Phase::Launching | Phase::Ending) {
                tx.save()?;
                return Err(ControlError::Unknown);
            }
            if matches!(record.phase, Phase::Running) {
                let result = self.result(record, false, false, cancelled)?;
                if result
                    .lease
                    .as_ref()
                    .is_some_and(|lease| lease.attachment != creation.attachment)
                {
                    return Err(ControlError::Busy);
                }
                let generation = record.generation;
                let _publication =
                    Self::admission(Some(record), owner, Effect::Resume, &mut admit)?;
                tx.state.operations.insert(
                    operation.clone(),
                    Operation {
                        owner: owner.clone(),
                        creation: creation.clone(),
                        generation,
                        created: false,
                        recreated: false,
                        create_only,
                    },
                );
                tx.retire_operations(None)?;
                return Ok(result);
            }
        } else if tx.state.sessions.len() >= MAX_SESSIONS {
            let retired: Vec<_> = tx
                .state
                .sessions
                .values()
                .filter(|record| matches!(record.phase, Phase::Absent))
                .map(|record| record.logical.clone())
                .collect();
            for logical in retired {
                tx.retire_record(&logical)?;
            }
            if tx.state.sessions.len() >= MAX_SESSIONS {
                return Err(ControlError::Busy);
            }
        }
        if tx
            .state
            .sessions
            .values()
            .filter(|record| matches!(record.phase, Phase::Launching))
            .count()
            >= MAX_PENDING_CREATES
        {
            return Err(ControlError::Busy);
        }
        let reservation = Self::admission(
            tx.state.sessions.get(logical),
            owner,
            Effect::Create,
            &mut admit,
        )?;
        let profile = profile(&creation.recipe)?;
        let fingerprint =
            detached::spawn_fingerprint(&profile).map_err(|_| ControlError::Invalid)?;
        // Native registries pass through serde_json::Value (u64 numbers).
        // Keep that compatibility while fencing clock rollback with the last
        // durable incarnation; the journal lock serializes generation selection.
        let last = tx
            .state
            .sessions
            .values()
            .map(|record| record.generation.get())
            .max()
            .unwrap_or(0)
            .max(tx.state.last_generation);
        let candidate = now_ms()
            .checked_mul(1_000_000)
            .ok_or(ControlError::Storage)?
            + uuid::Uuid::new_v4().as_u128() % 1_000_000;
        let value = candidate.max(last.checked_add(1).ok_or(ControlError::Storage)?);
        if value > u64::MAX as u128 {
            return Err(ControlError::Storage);
        }
        let generation = GenerationId::new(value).map_err(|_| ControlError::Invalid)?;
        let epoch = match tx.state.sessions.get(logical) {
            Some(record) => record.epoch.next().map_err(|_| ControlError::Storage)?,
            None => Epoch::new(1).map_err(|_| ControlError::Storage)?,
        };
        let recreated = tx.state.sessions.contains_key(logical);
        let record = Record {
            owner: owner.clone(),
            logical: logical.clone(),
            display_name: creation
                .name
                .clone()
                .unwrap_or_else(|| format!("Shell {}", &logical.as_str()[..8])),
            recipe: creation.recipe.clone(),
            effective_recipe: Recipe {
                shell: profile.executable().to_str().map(str::to_owned),
                args: profile
                    .arguments()
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect(),
                cwd: profile
                    .working_directory()
                    .and_then(Path::to_str)
                    .map(str::to_owned),
            },
            lifetime: creation.lifetime,
            epoch,
            revoked: false,
            name: native_name(owner, logical),
            launch: operation.clone(),
            generation,
            fingerprint,
            phase: Phase::Launching,
            identity: None,
        };
        tx.state.sessions.insert(logical.clone(), record);
        tx.state.last_generation = generation.get();
        tx.state.operations.insert(
            operation.clone(),
            Operation {
                owner: owner.clone(),
                creation: creation.clone(),
                generation,
                created: true,
                recreated,
                create_only,
            },
        );
        // Logical reservation, immutable intent and generation precede *all* child spawn.
        tx.save()?;
        let record = &tx.state.sessions[logical];
        let temporary =
            matches!(creation.lifetime, Lifetime::Temporary { .. }).then(|| TemporaryLease {
                lease: LeaseId::new(),
                epoch: Epoch::new(1).expect("nonzero epoch"),
                attachment: creation.attachment.clone(),
            });
        let lease = DaemonLease {
            version: 1,
            name: record.name.clone(),
            generation,
            revision: 1,
            lifetime: creation.lifetime,
            deadline_ms: temporary.as_ref().map(|_| now_ms() + 60_000),
            temporary,
            connected: false,
            stopped: false,
            remote_revoked: false,
            pending: None,
            last_attachment: None,
        };
        LeaseGuard::acquire(&self.root, generation)
            .and_then(|guard| guard.create(&lease))
            .map_err(|_| ControlError::CommittedButNotPublished)?;
        drop(reservation);
        let request = detached::DetachedSessionRequest {
            name: &record.name,
            profile: &profile,
            size: terminal_size(),
            scrollback_limit_bytes: 4096,
            launch_request: operation.as_str(),
        };
        let identity = detached::ensure_host_reserved(
            &self.root,
            request,
            generation.get(),
            true,
            &self.helper,
        )
        .map_err(|_| ControlError::Unknown)?;
        let record = tx
            .state
            .sessions
            .get_mut(logical)
            .ok_or(ControlError::Storage)?;
        record.identity = Some(Identity {
            pid: identity.pid,
            endpoint: identity.endpoint,
        });
        record.phase = Phase::Running;
        record.epoch = record.epoch.next().map_err(|_| ControlError::Storage)?;
        tx.save()
            .map_err(|_| ControlError::CommittedButNotPublished)?;
        tx.retire_operations(None)
            .map_err(|_| ControlError::CommittedButNotPublished)?;
        let record = &tx.state.sessions[logical];
        let _publication = Self::admission(Some(record), owner, Effect::Resume, &mut admit)
            .map_err(|_| ControlError::CommittedButNotPublished)?;
        self.result(record, true, recreated, cancelled)
    }

    fn summary(&self, record: &Record) -> Result<SessionSummary, ControlError> {
        let identity = record.native().ok_or(ControlError::Unknown)?;
        let attached = detached::host_generation_attached(&self.root, &identity)
            .map_err(|_| ControlError::Unknown)?;
        Ok(SessionSummary {
            target: record.target(),
            name: record.display_name.clone(),
            lifetime: record.lifetime,
            attached,
        })
    }

    fn result(
        &self,
        record: &Record,
        created: bool,
        recreated: bool,
        cancelled: &AtomicBool,
    ) -> Result<EnsureResult, ControlError> {
        match record.phase {
            Phase::Absent => return Err(ControlError::Ended),
            Phase::Launching | Phase::Ending => return Err(ControlError::Unknown),
            Phase::Running => {}
        }
        let guard = LeaseGuard::acquire(&self.root, record.generation).map_err(storage_error)?;
        let lease = guard.read().map_err(storage_error)?;
        if lease.expired() {
            return Err(ControlError::Ended);
        }
        Ok(EnsureResult {
            summary: self.summary(record)?,
            target: record.target(),
            identity: record.native().ok_or(ControlError::Unknown)?,
            lifetime: record.lifetime,
            lease: lease.temporary,
            created,
            recreated,
            cancelled: cancelled.load(Ordering::Acquire),
        })
    }

    fn exact<'a, G>(
        &self,
        tx: &'a mut Transaction,
        owner: &PrincipalId,
        target: &Target,
        effect: Effect,
        admit: &mut impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<(&'a mut Record, G), ControlError> {
        let record = tx
            .state
            .sessions
            .get_mut(&target.logical)
            .ok_or(ControlError::Denied)?;
        if &record.owner != owner {
            return Err(ControlError::Denied);
        }
        if record.revoked {
            return Err(ControlError::Revoked);
        }
        if record.generation != target.generation {
            return Err(ControlError::Stale);
        }
        self.reconcile(record)?;
        if !matches!(record.phase, Phase::Running) {
            return Err(ControlError::Unknown);
        }
        tx.save()?;
        let record = tx
            .state
            .sessions
            .get_mut(&target.logical)
            .ok_or(ControlError::Storage)?;
        let guard = Self::admission(Some(record), owner, effect, admit)?;
        Ok((record, guard))
    }

    /// Run one NONBLOCKING local relay action under current journal, daemon
    /// lease and fresh policy guards. No reconciliation, spawn, native IPC,
    /// summary construction or network wait occurs here.
    pub fn with_attachment_authorized<G, T>(
        &self,
        owner: &PrincipalId,
        target: &Target,
        binding: Option<&TemporaryLease>,
        admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
        action: impl FnOnce() -> Result<T, ControlError>,
    ) -> Result<T, ControlError> {
        self.with_authorized_lease(owner, target, admit, |lease| {
            if lease.temporary.as_ref() != binding {
                return Err(ControlError::Stale);
            }
            action()
        })
    }

    /// Authorizes pending recovery retrieval/adoption, NOT Attached success.
    /// The ticket must still name the live pending transfer, or that exact
    /// transfer must already have committed with the expected binding.
    pub fn with_pending_attachment_authorized<G, T>(
        &self,
        owner: &PrincipalId,
        target: &Target,
        ticket: &OperationId,
        expected: &TemporaryLease,
        admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
        action: impl FnOnce() -> Result<T, ControlError>,
    ) -> Result<T, ControlError> {
        self.with_authorized_lease(owner, target, admit, |lease| {
            let committed = lease.last_attachment.as_ref() == Some(ticket)
                && lease.temporary.as_ref() == Some(expected);
            let pending = lease.pending.as_ref().is_some_and(|pending| {
                &pending.ticket == ticket
                    && pending.deadline_ms > now_ms()
                    && pending.previous.lease == expected.lease
                    && pending.previous.epoch.next().ok() == Some(expected.epoch)
                    && pending.attachment == expected.attachment
            });
            if !committed && !pending {
                return Err(ControlError::Stale);
            }
            action()
        })
    }

    fn with_authorized_lease<G, T>(
        &self,
        owner: &PrincipalId,
        target: &Target,
        mut admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
        action: impl FnOnce(&DaemonLease) -> Result<T, ControlError>,
    ) -> Result<T, ControlError> {
        let tx = self.transaction()?;
        let record = tx
            .state
            .sessions
            .get(&target.logical)
            .ok_or(ControlError::Denied)?;
        if &record.owner != owner {
            return Err(ControlError::Denied);
        }
        if record.revoked {
            return Err(ControlError::Revoked);
        }
        if record.generation != target.generation {
            return Err(ControlError::Stale);
        }
        if !matches!(record.phase, Phase::Running) {
            return Err(ControlError::Unknown);
        }
        let _admission = Self::admission(Some(record), owner, Effect::Attach, &mut admit)?;
        let guard = LeaseGuard::acquire(&self.root, record.generation).map_err(storage_error)?;
        let lease = guard.read().map_err(storage_error)?;
        if lease.remote_revoked {
            return Err(ControlError::Revoked);
        }
        if lease.expired() {
            return Err(ControlError::Ended);
        }
        action(&lease)
    }

    /// Opens the real single-writer daemon admission. A temporary transfer and
    /// old-owner disconnect serialize on the same journal + daemon lease locks.
    #[allow(clippy::too_many_arguments)]
    pub fn attach<G>(
        &self,
        owner: &PrincipalId,
        target: &Target,
        attachment: &AttachmentId,
        takeover: bool,
        maximum_recovery_bytes: u32,
        notifier: Arc<dyn SessionEventNotifier>,
        mut admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<PendingAttachment, ControlError> {
        let mut tx = self.transaction()?;
        let (record, admission) = self.exact(&mut tx, owner, target, Effect::Attach, &mut admit)?;
        let guard = LeaseGuard::acquire(&self.root, record.generation).map_err(storage_error)?;
        let mut lease = guard.read().map_err(storage_error)?;
        if lease.expired() {
            return Err(ControlError::Ended);
        }

        let transfer = lease
            .temporary
            .as_ref()
            .is_some_and(|lease| &lease.attachment != attachment);
        if transfer && !takeover {
            return Err(ControlError::Busy);
        }
        let ticket = lease.temporary.as_ref().map(|_| OperationId::new());
        let mut reserved_lease = lease.temporary.clone();
        if let Some(binding) = reserved_lease.as_mut() {
            if lease
                .pending
                .as_ref()
                .is_some_and(|pending| pending.deadline_ms > now_ms())
            {
                return Err(ControlError::Busy);
            }
            lease.pending = Some(PendingLeaseAttachment {
                ticket: ticket.clone().ok_or(ControlError::Storage)?,
                previous: binding.clone(),
                attachment: attachment.clone(),
                deadline_ms: now_ms() + 15_000,
            });
            binding.epoch = binding.epoch.next().map_err(|_| ControlError::Storage)?;
            binding.attachment = attachment.clone();
            guard
                .replace(&mut lease)
                .map_err(|_| ControlError::CommittedButNotPublished)?;
        }
        drop(guard);
        drop(admission);
        let identity = record.native().ok_or(ControlError::Unknown)?;
        let connected = match &ticket {
            Some(ticket) => PersistentSession::resume_host_reserved_in(
                &identity,
                &self.root,
                takeover,
                maximum_recovery_bytes,
                ticket,
                notifier,
            ),
            None => PersistentSession::resume_remote_in(
                &identity,
                &self.root,
                takeover,
                maximum_recovery_bytes,
                notifier,
            ),
        };
        let session = match connected {
            Ok(session) => session,
            Err(_) => {
                if let Some(ticket) = &ticket {
                    let guard = LeaseGuard::acquire(&self.root, record.generation)
                        .map_err(|_| ControlError::CommittedButNotPublished)?;
                    let mut lease = guard
                        .read()
                        .map_err(|_| ControlError::CommittedButNotPublished)?;
                    if lease.last_attachment.as_ref() == Some(ticket) {
                        return Err(ControlError::CommittedButNotPublished);
                    }
                    if lease
                        .pending
                        .as_ref()
                        .is_some_and(|pending| &pending.ticket == ticket)
                    {
                        lease.pending = None;
                        guard
                            .replace(&mut lease)
                            .map_err(|_| ControlError::CommittedButNotPublished)?;
                    }
                }
                return Err(ControlError::Busy);
            }
        };
        let _publication = Self::admission(Some(record), owner, Effect::Attach, &mut admit)
            .map_err(|_| ControlError::CommittedButNotPublished)?;
        Ok(PendingAttachment {
            session,
            ticket,
            reserved_lease,
        })
    }

    /// Poll after the frontend has adopted the deferred recovery snapshot.
    /// Busy means not committed, not successful ownership or readiness.
    pub fn attachment_outcome<G>(
        &self,
        owner: &PrincipalId,
        target: &Target,
        ticket: &OperationId,
        expected: &TemporaryLease,
        mut admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<TemporaryLease, ControlError> {
        let mut tx = self.transaction()?;
        let (record, _admission) =
            self.exact(&mut tx, owner, target, Effect::Attach, &mut admit)?;
        let guard = LeaseGuard::acquire(&self.root, record.generation).map_err(storage_error)?;
        let lease = guard.read().map_err(storage_error)?;
        if lease.expired() {
            return Err(ControlError::Ended);
        }
        if lease
            .pending
            .as_ref()
            .is_some_and(|pending| &pending.ticket == ticket)
        {
            return Err(
                if lease
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.deadline_ms > now_ms())
                {
                    ControlError::Busy
                } else {
                    ControlError::Stale
                },
            );
        }
        if lease.last_attachment.as_ref() != Some(ticket)
            || lease.temporary.as_ref() != Some(expected)
        {
            return Err(ControlError::Stale);
        }
        Ok(expected.clone())
    }

    /// Renew at intervals shorter than 60s while the owning channel is alive.
    /// A heartbeat is lease maintenance, never shell input or a reconnect.
    pub fn maintain_lease<G>(
        &self,
        owner: &PrincipalId,
        target: &Target,
        binding: &TemporaryLease,
        disconnect: Option<DetachKind>,
        mut admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<(), ControlError> {
        let mut tx = self.transaction()?;
        let (record, _admission) = self.exact(&mut tx, owner, target, Effect::Lease, &mut admit)?;
        let guard = LeaseGuard::acquire(&self.root, record.generation).map_err(storage_error)?;
        let mut lease = guard.read().map_err(storage_error)?;
        if lease.temporary.as_ref() != Some(binding) {
            return Err(ControlError::Stale);
        }
        if lease.expired() {
            return Err(ControlError::Ended);
        }
        match disconnect {
            Some(DetachKind::Intentional) => lease.stopped = true,
            // A missing bridge heartbeat already starts the same bounded grace.
            Some(DetachKind::TransportLost) if lease.connected => {
                lease.connected = false;
                lease.deadline_ms = Some(now_ms() + 60_000);
            }
            Some(DetachKind::TransportLost) => {}
            None if lease.connected => lease.deadline_ms = Some(now_ms() + 60_000),
            None => return Err(ControlError::Stale),
        }
        guard.replace(&mut lease).map_err(storage_error)
    }

    /// Dropping a pending frontend does not imply its adoption failed. Cancel
    /// under the same lease lock as daemon commit and return the actual outcome.
    pub fn cancel_attachment<G>(
        &self,
        owner: &PrincipalId,
        target: &Target,
        ticket: &OperationId,
        mut admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<AttachmentCancellation, ControlError> {
        let mut tx = self.transaction()?;
        let (record, _admission) =
            self.exact(&mut tx, owner, target, Effect::Attach, &mut admit)?;
        let guard = LeaseGuard::acquire(&self.root, record.generation).map_err(storage_error)?;
        let mut lease = guard.read().map_err(storage_error)?;
        if lease.last_attachment.as_ref() == Some(ticket) {
            return lease
                .temporary
                .map(AttachmentCancellation::Committed)
                .ok_or(ControlError::Stale);
        }
        if lease
            .pending
            .as_ref()
            .is_some_and(|pending| &pending.ticket == ticket)
        {
            lease.pending = None;
            guard.replace(&mut lease).map_err(storage_error)?;
            return Ok(AttachmentCancellation::CancelledBeforeCommit);
        }
        Err(ControlError::Stale)
    }

    /// Requests exact-generation termination in the daemon's own loop. Success
    /// means durable stop intent, NOT observed process exit; Lookup reconciles it.
    pub fn end<G>(
        &self,
        owner: &PrincipalId,
        target: &Target,
        mut admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<EndOutcome, ControlError> {
        let mut tx = self.transaction()?;
        let (record, _admission) = self.exact(&mut tx, owner, target, Effect::End, &mut admit)?;
        let guard = LeaseGuard::acquire(&self.root, record.generation).map_err(storage_error)?;
        let mut lease = guard.read().map_err(storage_error)?;
        lease.stopped = true;
        guard.replace(&mut lease).map_err(storage_error)?;
        record.phase = Phase::Ending;
        record.epoch = record.epoch.next().map_err(|_| ControlError::Storage)?;
        tx.save()?;
        Ok(EndOutcome::StopRequested)
    }

    /// Protected LOCAL consent revocation only, not a remotely callable admin
    /// operation. Persistent shells survive; temporary leases retain their cap.
    pub fn revoke_local(
        &self,
        owner: &PrincipalId,
        logical: &LogicalSessionId,
    ) -> Result<(), ControlError> {
        let mut tx = self.transaction()?;
        let record = tx
            .state
            .sessions
            .get_mut(logical)
            .ok_or(ControlError::Denied)?;
        if &record.owner != owner {
            return Err(ControlError::Denied);
        }
        let guard = LeaseGuard::acquire(&self.root, record.generation).map_err(storage_error)?;
        let mut lease = guard.read().map_err(storage_error)?;
        lease.pending = None;
        lease.remote_revoked = true;
        guard.replace(&mut lease).map_err(storage_error)?;
        record.epoch = record.epoch.next().map_err(|_| ControlError::Storage)?;
        record.revoked = true;
        tx.save()
    }

    /// Protected LOCAL maintenance, never a wire administrative operation.
    /// Retires a known absent identity or explicitly abandons an uncertain
    /// creation. Abandonment seals its daemon lease but does NOT assert absence.
    /// The logical ID and all operation receipts remain permanently fenced.
    pub fn retire_local(&self, owner: &PrincipalId, target: &Target) -> Result<(), ControlError> {
        let mut tx = self.transaction()?;
        if let Some(retired) = tx.retired(&target.logical)? {
            return if &retired.owner != owner {
                Err(ControlError::Denied)
            } else if retired.generation != target.generation {
                Err(ControlError::Stale)
            } else {
                Ok(())
            };
        }
        let record = tx
            .state
            .sessions
            .get(&target.logical)
            .ok_or(ControlError::Denied)?;
        if &record.owner != owner {
            return Err(ControlError::Denied);
        }
        if record.generation != target.generation {
            return Err(ControlError::Stale);
        }
        if !record.revoked && !matches!(record.phase, Phase::Absent | Phase::Launching) {
            return Err(ControlError::Busy);
        }
        if matches!(record.phase, Phase::Launching) {
            let _start = detached::lock_native_start_in(&self.root, &record.name)
                .map_err(|_| ControlError::Busy)?;
            let guard =
                LeaseGuard::acquire(&self.root, record.generation).map_err(storage_error)?;
            let mut lease = match guard.read() {
                Ok(lease) => lease,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let temporary =
                        matches!(record.lifetime, Lifetime::Temporary { .. }).then(|| {
                            TemporaryLease {
                                lease: LeaseId::new(),
                                epoch: Epoch::new(1).expect("nonzero epoch"),
                                attachment: AttachmentId::new(),
                            }
                        });
                    let lease = DaemonLease {
                        version: 1,
                        name: record.name.clone(),
                        generation: record.generation,
                        revision: 1,
                        lifetime: record.lifetime,
                        deadline_ms: temporary.as_ref().map(|_| now_ms()),
                        temporary,
                        connected: false,
                        stopped: true,
                        remote_revoked: true,
                        pending: None,
                        last_attachment: None,
                    };
                    guard.create(&lease).map_err(storage_error)?;
                    lease
                }
                Err(error) => return Err(storage_error(error)),
            };
            lease.stopped = true;
            lease.remote_revoked = true;
            lease.pending = None;
            guard.replace(&mut lease).map_err(storage_error)?;
        }
        tx.retire_record(&target.logical)
    }
}

fn native_name(owner: &PrincipalId, logical: &LogicalSessionId) -> String {
    let mut digest = Sha256::new();
    digest.update(b"festerm-host-session-v1\0");
    digest.update(owner.as_str());
    digest.update(logical.as_str());
    format!("hs-{:x}", digest.finalize())[..51].into()
}

fn terminal_size() -> TerminalSize {
    TerminalSize::new(80, 24).expect("valid terminal size")
}

fn profile(recipe: &Recipe) -> Result<LocalProfile, ControlError> {
    // Resolve the host default once for a fresh creation. Stored request intent
    // stays structural; existing shells never change when a profile is edited.
    let shell = recipe
        .shell
        .as_ref()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os(if cfg!(windows) { "COMSPEC" } else { "SHELL" }).map(PathBuf::from)
        })
        .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "cmd.exe" } else { "/bin/sh" }));
    let mut profile =
        LocalProfile::new(shell).with_arguments(recipe.args.iter().map(std::ffi::OsString::from));
    if let Some(cwd) = &recipe.cwd {
        profile = profile.with_working_directory(cwd);
    }
    detached::normalize_spawn_profile(&profile).map_err(|_| ControlError::Invalid)
}
