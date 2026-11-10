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

fn storage_error(_: io::Error) -> ControlError {
    ControlError::Storage
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

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u8,
    sessions: BTreeMap<LogicalSessionId, Record>,
    operations: BTreeMap<OperationId, Operation>,
}

struct Transaction {
    directory: File,
    _lock: File,
    state: State,
}

impl Transaction {
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
        Ok(Transaction {
            directory,
            _lock: lock,
            state,
        })
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

    fn reconcile(&self, record: &mut Record) -> Result<(), ControlError> {
        if matches!(record.phase, Phase::Absent) {
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
                Err(_) => {
                    if !matches!(record.phase, Phase::Ending) {
                        record.phase = Phase::Launching;
                    }
                }
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
                tx.save()?;
                return Ok(result);
            }
        } else if tx.state.sessions.len() >= MAX_SESSIONS {
            return Err(ControlError::Busy);
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
            .unwrap_or(0);
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
        let session = match &ticket {
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
        }
        .map_err(|_| ControlError::Busy)?;
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
