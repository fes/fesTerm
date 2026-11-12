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
    AttachmentId, Creation, DetachKind, Epoch, GenerationId, IssuedOperation, LeaseId, Lifetime,
    LogicalSessionId, LogicalState, OperationId, PrincipalId, Recipe, SessionGrant, SessionSummary,
    Target, TemporaryLease, Validate, MAX_IDEMPOTENCY_RECORDS, MAX_PENDING_CREATES, MAX_SESSIONS,
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
#[path = "host_binding.rs"]
mod binding;
pub use binding::ImportedBinding;

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

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy)]
enum AttachFailureDiagnostic<'a> {
    ReservePublish(&'a io::Error),
    NativeConnect(&'a crate::PersistentSessionError),
    ConnectedPersistentRefusal,
    UnwindAcquire(&'a io::Error),
    UnwindRead(&'a io::Error),
    UnwindCommitted,
    UnwindPublish(&'a io::Error),
    PublicationReadmit(ControlError),
}

#[cfg(any(test, feature = "test-support"))]
fn attach_failure_diagnostics_enabled(
    temporary: bool,
    takeover: bool,
    previously_attached: bool,
) -> bool {
    temporary || (!takeover && previously_attached)
}

#[cfg(any(test, feature = "test-support"))]
impl AttachFailureDiagnostic<'_> {
    fn message(self, enabled: bool) -> Option<String> {
        if !enabled {
            return None;
        }
        if let Self::NativeConnect(error) = self {
            return Some(match error.native_failure {
                Some(failure) => format!(
                    "host attach boundary=native-connect phase={:?} io_kind={:?} raw_os={:?}",
                    failure.boundary, failure.io_kind, failure.raw_os
                ),
                None => "host attach boundary=native-connect phase=Unclassified".into(),
            });
        }
        let uncertain = ControlError::CommittedButNotPublished;
        let (step, error, control) = match self {
            Self::ReservePublish(error) => ("reserve-publish", Some(error), uncertain),
            Self::UnwindAcquire(error) => ("unwind-acquire", Some(error), uncertain),
            Self::UnwindRead(error) => ("unwind-read", Some(error), uncertain),
            Self::UnwindCommitted => ("unwind-already-committed", None, uncertain),
            Self::UnwindPublish(error) => ("unwind-publish", Some(error), uncertain),
            Self::PublicationReadmit(control) => ("publication-readmit", None, control),
            Self::ConnectedPersistentRefusal => {
                ("connected-persistent-refusal", None, ControlError::Busy)
            }
            Self::NativeConnect(_) => unreachable!("handled primary failure"),
        };
        Some(match error {
            Some(error) => format!(
                "host attach boundary={step} control={control:?} io_kind={:?} raw_os={:?}",
                error.kind(),
                error.raw_os_error()
            ),
            None => format!("host attach boundary={step} control={control:?}"),
        })
    }

    fn report(self, enabled: bool) {
        if let Some(message) = self.message(enabled) {
            eprintln!("{message}");
        }
    }
}

#[cfg(test)]
mod attach_diagnostic_tests {
    use super::*;

    #[test]
    fn native_attachment_failure_diagnostics_include_temporary_takeover() {
        for temporary in [false, true] {
            for takeover in [false, true] {
                for previously_attached in [false, true] {
                    let enabled = attach_failure_diagnostics_enabled(
                        temporary,
                        takeover,
                        previously_attached,
                    );
                    assert_eq!(enabled, temporary || (!takeover && previously_attached));
                    assert_eq!(
                        AttachFailureDiagnostic::UnwindCommitted
                            .message(enabled)
                            .is_some(),
                        enabled
                    );
                }
            }
        }
    }

    #[test]
    fn native_attachment_failure_diagnostics_are_redacted_and_boundary_exact() {
        let error = io::Error::new(
            io::ErrorKind::PermissionDenied,
            "SECRET /private/path account fingerprint receipt operation key input credential",
        );
        for (diagnostic, step) in [
            (
                AttachFailureDiagnostic::ReservePublish(&error),
                "reserve-publish",
            ),
            (
                AttachFailureDiagnostic::UnwindAcquire(&error),
                "unwind-acquire",
            ),
            (AttachFailureDiagnostic::UnwindRead(&error), "unwind-read"),
            (
                AttachFailureDiagnostic::UnwindPublish(&error),
                "unwind-publish",
            ),
        ] {
            assert!(diagnostic.message(false).is_none());
            assert_eq!(
                diagnostic.message(true).unwrap(),
                format!("host attach boundary={step} control=CommittedButNotPublished io_kind=PermissionDenied raw_os=None")
            );
        }
        assert_eq!(
            AttachFailureDiagnostic::UnwindCommitted
                .message(true)
                .unwrap(),
            "host attach boundary=unwind-already-committed control=CommittedButNotPublished"
        );
        assert_eq!(
            AttachFailureDiagnostic::PublicationReadmit(ControlError::Denied)
                .message(true)
                .unwrap(),
            "host attach boundary=publication-readmit control=Denied"
        );
        assert_eq!(
            AttachFailureDiagnostic::ConnectedPersistentRefusal
                .message(true)
                .unwrap(),
            "host attach boundary=connected-persistent-refusal control=Busy"
        );
        let os_error = io::Error::from_raw_os_error(5);
        assert_eq!(
            AttachFailureDiagnostic::ReservePublish(&os_error).message(true).unwrap(),
            format!("host attach boundary=reserve-publish control=CommittedButNotPublished io_kind={:?} raw_os=Some(5)", os_error.kind())
        );
        let native_error = crate::PersistentSessionError::new("SECRET /private/path")
            .at_native_boundary(crate::NativeConnectionBoundary::AdmissionRead, Some(&error));
        assert!(AttachFailureDiagnostic::NativeConnect(&native_error)
            .message(false)
            .is_none());
        assert_eq!(
            AttachFailureDiagnostic::NativeConnect(&native_error)
                .message(true)
                .unwrap(),
            "host attach boundary=native-connect phase=AdmissionRead io_kind=Some(PermissionDenied) raw_os=None"
        );
        let refused = crate::PersistentSessionError::new("SECRET /private/path")
            .at_native_boundary(crate::NativeConnectionBoundary::AdmissionRefused, None);
        assert_eq!(
            AttachFailureDiagnostic::NativeConnect(&refused)
                .message(true)
                .unwrap(),
            "host attach boundary=native-connect phase=AdmissionRefused io_kind=None raw_os=None"
        );
    }

    #[test]
    fn failed_native_attach_unwind_contention_preserves_committed_writer_and_exact_pending_ticket()
    {
        assert_failed_native_attach_unwind(Lifetime::Persistent {});
    }

    #[test]
    fn failed_native_attach_unwind_preserves_temporary_ownership_and_exact_pending_ticket() {
        assert_failed_native_attach_unwind(Lifetime::Temporary {
            reconnect_grace_seconds: 60,
        });
    }

    fn assert_failed_native_attach_unwind(lifetime: Lifetime) {
        let directory = storage::private_test_directory();
        let root = directory.path();
        let storage_directory = storage::private_directory(root, false).unwrap();
        let generation = GenerationId::new(1).unwrap();
        let old_ticket = OperationId::new();
        let ticket = OperationId::new();
        let replacement_attachment = AttachmentId::new();
        let temporary = matches!(lifetime, Lifetime::Temporary { .. }).then(|| TemporaryLease {
            lease: LeaseId::new(),
            epoch: Epoch::new(4).unwrap(),
            attachment: AttachmentId::new(),
        });
        let mut lease = DaemonLease {
            version: 1,
            name: "fixture".into(),
            generation,
            revision: 1,
            lifetime,
            temporary: temporary.clone(),
            deadline_ms: temporary.as_ref().map(|_| now_ms() + 60_000),
            connected: temporary.is_some(),
            stopped: false,
            remote_revoked: false,
            pending: Some(PendingLeaseAttachment {
                ticket: ticket.clone(),
                writer_epoch: Epoch::new(2).unwrap(),
                previous: temporary.clone(),
                attachment: replacement_attachment.clone(),
                deadline_ms: now_ms() + 15_000,
            }),
            last_attachment: Some(old_ticket.clone()),
            grant: SessionGrant {
                owner: PrincipalId::new(),
                logical: LogicalSessionId::new(),
                revision: Epoch::new(1).unwrap(),
                remote_access: true,
                state: LogicalState::Running(generation),
            },
            writer_epoch: Epoch::new(2).unwrap(),
            native_pid: Some(std::process::id()),
        };
        let old = AttachmentAuthority {
            root: root.into(),
            native_root: root.into(),
            binding: None,
            target: Target {
                logical: lease.grant.logical.clone(),
                generation,
            },
            identity: NativeSessionIdentity {
                name: lease.name.clone(),
                pid: std::process::id(),
                created_at_unix_ms: generation.get(),
                endpoint: "fixture".into(),
            },
            grant: lease.grant.clone(),
            writer_epoch: lease.writer_epoch,
            ticket: old_ticket,
            expected: temporary,
        };
        let guard = LeaseGuard::reserve(root, generation).unwrap();
        guard.create(&lease).unwrap();
        let name = Path::new("host-daemon-1.json");
        let before = storage::read_private(&storage_directory, name, 4096).unwrap();
        assert_eq!(
            SessionControl::unwind_attachment(root, generation, &ticket, true),
            Err(ControlError::CommittedButNotPublished)
        );
        assert_eq!(
            storage::read_private(&storage_directory, name, 4096).unwrap(),
            before
        );
        let held = guard.read().unwrap();
        assert_eq!(held.pending.as_ref().unwrap().ticket, ticket);
        assert_eq!(held.pending.as_ref().unwrap().previous, lease.temporary);
        assert_eq!(
            held.pending.as_ref().unwrap().attachment,
            replacement_attachment
        );
        if let Some(binding) = held.temporary.as_ref() {
            assert_ne!(binding.attachment, replacement_attachment);
        }
        assert_eq!(held.temporary, lease.temporary);
        assert_eq!(held.deadline_ms, lease.deadline_ms);
        assert_eq!(held.connected, lease.connected);
        assert!(same_grant(&held.grant, &lease.grant));
        assert_eq!(held.writer_epoch, old.writer_epoch);
        SessionControl::committed(&old, &held).unwrap();
        drop(guard);
        SessionControl::unwind_attachment(root, generation, &OperationId::new(), true).unwrap();
        let guard = LeaseGuard::acquire(root, generation).unwrap();
        assert_eq!(
            storage::read_private(&storage_directory, name, 4096).unwrap(),
            before
        );
        drop(guard);
        SessionControl::unwind_attachment(root, generation, &ticket, true).unwrap();
        let guard = LeaseGuard::acquire(root, generation).unwrap();
        let cleared = guard.read().unwrap();
        assert!(cleared.pending.is_none());
        let mut expected = lease.clone();
        expected.pending = None;
        expected.revision += 1;
        assert_eq!(
            serde_json::to_value(&cleared).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        assert_eq!(cleared.writer_epoch, lease.writer_epoch);
        assert_eq!(cleared.last_attachment, lease.last_attachment);
        assert_eq!(cleared.temporary, lease.temporary);
        assert_eq!(cleared.deadline_ms, lease.deadline_ms);
        assert_eq!(cleared.connected, lease.connected);
        assert!(same_grant(&cleared.grant, &lease.grant));
        SessionControl::committed(&old, &cleared).unwrap();
        lease = cleared;
        lease.last_attachment = Some(ticket.clone());
        lease.writer_epoch = lease.writer_epoch.next().unwrap();
        if let Some(binding) = lease.temporary.as_mut() {
            binding.epoch = binding.epoch.next().unwrap();
            binding.attachment = replacement_attachment;
        }
        guard.replace(&mut lease).unwrap();
        let committed = storage::read_private(&storage_directory, name, 4096).unwrap();
        drop(guard);
        assert_eq!(
            SessionControl::unwind_attachment(root, generation, &ticket, true),
            Err(ControlError::CommittedButNotPublished)
        );
        let guard = LeaseGuard::acquire(root, generation).unwrap();
        let current = guard.read().unwrap();
        assert_eq!(current.last_attachment.as_ref(), Some(&ticket));
        assert_eq!(current.writer_epoch, old.writer_epoch.next().unwrap());
        assert_eq!(
            storage::read_private(&storage_directory, name, 4096).unwrap(),
            committed
        );
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
    /// Some only inside a protected control transaction. Per-generation relay
    /// actions do not read or advertise the global creation namespace.
    pub issuance_epoch: Option<Epoch>,
}

fn scoped_issuance<G>(
    epoch: Epoch,
    mut admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
) -> impl FnMut(&AdmissionState) -> Result<G, ControlError> {
    move |state| {
        admit(&AdmissionState {
            grant: state.grant.clone(),
            grant_epoch: state.grant_epoch,
            effect: state.effect,
            issuance_epoch: Some(epoch),
        })
    }
}

fn same_grant(left: &SessionGrant, right: &SessionGrant) -> bool {
    left.logical == right.logical
        && left.owner == right.owner
        && left.remote_access == right.remote_access
        && left.state == right.state
        && left.revision == right.revision
}

fn attachment_storage_error(boundary: &'static str, error: io::Error) -> ControlError {
    #[cfg(any(test, feature = "test-support"))]
    let detail = (error.kind(), error.raw_os_error());
    let control = storage_error(error);
    #[cfg(any(test, feature = "test-support"))]
    eprintln!(
        "host attachment boundary={boundary} control={control:?} io_kind={:?} raw_os={:?}",
        detail.0, detail.1
    );
    #[cfg(not(any(test, feature = "test-support")))]
    let _ = boundary;
    control
}

#[derive(Clone, Debug)]
pub struct EnsureResult {
    pub summary: SessionSummary,
    pub target: Target,
    pub identity: NativeSessionIdentity,
    pub lifetime: Lifetime,
    pub lease: Option<TemporaryLease>,
    /// Cleanup of an unadopted temporary creation only, never terminal access.
    pub initial_lease_authority: Option<InitialLeaseAuthority>,
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
    pub authority: AttachmentAuthority,
}

/// Minted only by protected native admission. Cloning cannot renew its writer
/// epoch; every use checks the generation's current protected fences.
#[derive(Clone, Debug)]
pub struct AttachmentAuthority {
    root: PathBuf,
    native_root: PathBuf,
    binding: Option<crate::native_binding::Binding>,
    target: Target,
    identity: NativeSessionIdentity,
    grant: SessionGrant,
    writer_epoch: Epoch,
    ticket: OperationId,
    expected: Option<TemporaryLease>,
}

#[derive(Clone, Debug)]
pub struct InitialLeaseAuthority {
    root: PathBuf,
    target: Target,
    grant: SessionGrant,
    binding: TemporaryLease,
}

impl AttachmentAuthority {
    pub fn target(&self) -> &Target {
        &self.target
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttachmentCancellation {
    CancelledBeforeCommit,
    Committed(Option<TemporaryLease>),
}

#[derive(Clone, Debug)]
pub struct DetachmentReceipt {
    authority: AttachmentAuthority,
    revision: u64,
    kind: DetachKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndOutcome {
    StopRequested,
    ObservedEnded,
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

pub struct ProtectedUnknown {
    pub grant: SessionGrant,
    pub target: Target,
}

pub struct ProtectedInventory {
    pub sessions: Vec<ProtectedSession>,
    pub unknown: Vec<ProtectedUnknown>,
}

pub struct ProtectedLookup {
    pub state: Lookup,
    pub grant: Option<SessionGrant>,
    pub summary: Option<SessionSummary>,
}

pub struct LocalObservation {
    pub target: Target,
    pub revision: Epoch,
    pub revoked: bool,
    pub state: Lookup,
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
    artifacts_retired: bool,
    #[serde(default)]
    binding: Option<ImportedBinding>,
}

impl Record {
    fn root<'a>(&'a self, root: &'a Path) -> &'a Path {
        self.binding
            .as_ref()
            .map_or(root, |binding| binding.binding.runtime_root.as_path())
    }
    fn grant(&self) -> SessionGrant {
        SessionGrant {
            logical: self.logical.clone(),
            owner: self.owner.clone(),
            revision: self.epoch,
            remote_access: !self.revoked,
            state: self.state(),
        }
    }

    fn lease_matches(&self, lease: &DaemonLease) -> bool {
        lease.name == self.name
            && lease.grant.owner == self.owner
            && lease.grant.logical == self.logical
            && lease.lifetime == self.lifetime
            && self
                .identity
                .as_ref()
                .is_none_or(|identity| lease.native_pid == Some(identity.pid))
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
    logical: LogicalSessionId,
    intent: String,
    generation: GenerationId,
    created: bool,
    recreated: bool,
}

impl Operation {
    fn intent(
        owner: &PrincipalId,
        creation: &Creation,
        create_only: bool,
    ) -> Result<String, ControlError> {
        let bytes = serde_json::to_vec(&(owner, creation, create_only))
            .map_err(|_| ControlError::Storage)?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u8,
    issuance_epoch: Epoch,
    sessions: BTreeMap<LogicalSessionId, Record>,
    operations: BTreeMap<OperationId, Operation>,
    last_generation: u128,
    #[serde(default)]
    namespace: Option<OperationId>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: 2,
            issuance_epoch: Epoch::new(1).expect("nonzero epoch"),
            sessions: BTreeMap::new(),
            operations: BTreeMap::new(),
            last_generation: 0,
            namespace: Some(OperationId::new()),
        }
    }
}

struct Transaction {
    directory: File,
    _lock: File,
    state: State,
}

struct NativeAdmission<G> {
    _policy: G,
    _source: Option<crate::native_binding::SourceFence>,
}

impl Transaction {
    fn rotate_issuance(&mut self) -> Result<(), ControlError> {
        self.state.issuance_epoch = self
            .state
            .issuance_epoch
            .next()
            .map_err(|_| ControlError::Storage)?;
        self.state.operations.clear();
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

#[cfg(all(test, unix))]
mod end_confirmation_tests {
    use super::*;

    fn observed_fixture() -> (
        storage::PrivateTestDirectory,
        SessionControl,
        PrincipalId,
        Target,
    ) {
        let directory = storage::private_test_directory();
        storage::private_directory(directory.path(), false).unwrap();
        let control = SessionControl {
            root: directory.path().into(),
            helper: directory.path().join("never-executed-helper"),
        };
        let owner = PrincipalId::new();
        let target = Target {
            logical: LogicalSessionId::new(),
            generation: GenerationId::new(1).unwrap(),
        };
        let record = Record {
            owner: owner.clone(),
            logical: target.logical.clone(),
            display_name: "controlled".into(),
            recipe: Recipe::default(),
            effective_recipe: Recipe::default(),
            lifetime: Lifetime::Persistent {},
            epoch: Epoch::new(1).unwrap(),
            revoked: false,
            name: native_slot(0),
            launch: OperationId::new(),
            generation: target.generation,
            fingerprint: "0".repeat(64),
            phase: Phase::Absent,
            identity: Some(Identity {
                pid: std::process::id(),
                endpoint: "controlled".into(),
            }),
            artifacts_retired: false,
            binding: None,
        };
        let lease = DaemonLease {
            version: 1,
            name: record.name.clone(),
            generation: record.generation,
            revision: 1,
            lifetime: record.lifetime,
            temporary: None,
            deadline_ms: None,
            connected: false,
            stopped: true,
            remote_revoked: false,
            pending: None,
            last_attachment: None,
            grant: record.grant(),
            writer_epoch: Epoch::new(1).unwrap(),
            native_pid: Some(std::process::id()),
        };
        LeaseGuard::reserve(&control.root, target.generation)
            .unwrap()
            .create(&lease)
            .unwrap();
        let mut tx = control.transaction().unwrap();
        tx.state.last_generation = target.generation.get();
        tx.state.sessions.insert(target.logical.clone(), record);
        tx.save().unwrap();
        drop(tx);
        (directory, control, owner, target)
    }

    #[test]
    fn end_confirmation_requires_fresh_policy_without_storing_observation() {
        let (_directory, control, owner, target) = observed_fixture();
        for refusal in [
            ControlError::Unknown,
            ControlError::Busy,
            ControlError::Denied,
            ControlError::Revoked,
            ControlError::Stale,
        ] {
            assert_eq!(
                control.end(&owner, &target, |_| Err::<(), _>(refusal)),
                Err(refusal)
            );
        }
        assert_eq!(
            control.end(&owner, &target, |state| {
                assert_eq!(state.effect, Effect::End);
                assert_eq!(
                    state.grant.as_ref().unwrap().state,
                    LogicalState::Ended(target.generation)
                );
                assert!(state.issuance_epoch.is_some());
                let error = LeaseGuard::acquire(&control.root, target.generation)
                    .err()
                    .expect("End observation must retain the native lease fence");
                assert!(crate::lock_is_contended(&error));
                Ok(())
            }),
            Ok(EndOutcome::ObservedEnded)
        );
        assert_eq!(
            control.transaction().unwrap().state.sessions[&target.logical].state(),
            LogicalState::Absent
        );
        assert_eq!(
            LeaseGuard::acquire(&control.root, target.generation)
                .unwrap()
                .read()
                .unwrap()
                .grant
                .state,
            LogicalState::Absent
        );
    }

    #[test]
    fn end_confirmation_refuses_foreign_revoked_replaced_unbound_retired_or_missing_records() {
        for field in 0..6 {
            let (_directory, control, mut owner, mut target) = observed_fixture();
            let mut tx = control.transaction().unwrap();
            let record = tx.state.sessions.get_mut(&target.logical).unwrap();
            let expected = match field {
                0 => {
                    owner = PrincipalId::new();
                    ControlError::Denied
                }
                1 => {
                    record.revoked = true;
                    ControlError::Revoked
                }
                2 => {
                    target.generation = GenerationId::new(2).unwrap();
                    ControlError::Stale
                }
                3 => {
                    record.identity = None;
                    ControlError::Unknown
                }
                4 => {
                    record.artifacts_retired = true;
                    ControlError::Unknown
                }
                _ => {
                    tx.state.sessions.remove(&target.logical);
                    ControlError::Denied
                }
            };
            tx.save().unwrap();
            drop(tx);
            assert_eq!(
                control.end(&owner, &target, |_| -> Result<(), ControlError> {
                    panic!("an unproved generation must not reach End admission");
                }),
                Err(expected)
            );
        }
    }

    #[test]
    fn end_confirmation_refuses_contended_or_corrupt_native_lease() {
        let (_directory, control, owner, target) = observed_fixture();
        let guard = LeaseGuard::acquire(&control.root, target.generation).unwrap();
        assert_eq!(
            control.end(&owner, &target, |_| Ok(())),
            Err(ControlError::Busy)
        );
        drop(guard);
        let directory = storage::private_directory(&control.root, false).unwrap();
        storage::replace_private(&directory, Path::new("host-daemon-1.json"), b"invalid").unwrap();
        assert_eq!(
            control.end(&owner, &target, |_| Ok(())),
            Err(ControlError::Unknown)
        );
    }
}

impl SessionControl {
    /// Binding/observation only: this handle cannot create or recreate shells.
    pub fn existing_only(root: &Path) -> Result<Self, ControlError> {
        if !root.is_absolute() {
            return Err(ControlError::Invalid);
        }
        detached::prepare_native_runtime(root).map_err(|_| ControlError::Storage)?;
        Ok(Self {
            root: root.into(),
            helper: PathBuf::new(),
        })
    }

    /// Lazy non-launching handle. No filesystem effects; trusted composition
    /// must gate namespace initialization with current protected consent.
    pub fn retained_namespace(root: &Path) -> Result<Self, ControlError> {
        if !root.is_absolute() {
            return Err(ControlError::Invalid);
        }
        Ok(Self {
            root: root.into(),
            helper: PathBuf::new(),
        })
    }

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
        let (lock, initialized) =
            match storage::open_private_file(&directory, Path::new("host-sessions.lock"), false) {
                Ok(file) => (file, false),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    // A missing half of an existing journal is not a new
                    // namespace. Never reset issuance or split a live lock.
                    match storage::read_private(&directory, Path::new(STATE_FILE), STATE_LIMIT) {
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        _ => return Err(ControlError::Storage),
                    }
                    match storage::open_private_file(
                        &directory,
                        Path::new("host-sessions.lock"),
                        true,
                    ) {
                        Ok(file) => (file, true),
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (
                            storage::open_private_file(
                                &directory,
                                Path::new("host-sessions.lock"),
                                false,
                            )
                            .map_err(storage_error)?,
                            false,
                        ),
                        Err(error) => return Err(storage_error(error)),
                    }
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
            Err(error) if error.kind() == io::ErrorKind::NotFound && initialized => {
                State::default()
            }
            Err(error) => return Err(storage_error(error)),
        };
        match storage::read_private(&directory, Path::new("host-binding-namespace.json"), 256) {
            Ok(bytes)
                if state.namespace.as_ref().is_some_and(|namespace| {
                    serde_json::from_slice::<OperationId>(&bytes).ok().as_ref() == Some(namespace)
                }) => {}
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    && !state
                        .sessions
                        .values()
                        .any(|record| record.binding.is_some()) => {}
            _ => return Err(ControlError::Storage),
        }
        if state.version != 2
            || state.sessions.len() > MAX_SESSIONS
            || state.operations.len() > MAX_IDEMPOTENCY_RECORDS
            || state.last_generation > u64::MAX as u128
        {
            return Err(ControlError::Storage);
        }
        for (logical, record) in &state.sessions {
            if logical != &record.logical
                || (record.binding.is_none()
                    && !(0..MAX_SESSIONS).any(|slot| record.name == native_slot(slot)))
                || record.binding.as_ref().is_some_and(|binding| {
                    binding.binding.validate().is_err()
                        || binding.binding.logical != record.logical
                        || Some(&binding.binding.namespace) != state.namespace.as_ref()
                        || binding.binding.control_root != self.root
                        || binding.binding.principal != record.owner
                        || binding.binding.operation != record.launch
                        || binding.binding.spawn_fingerprint != record.fingerprint
                        || binding.binding.issuance_epoch.get() > state.issuance_epoch.get()
                        || binding.acknowledged && !binding.lease_reserved
                        || binding.activated && !binding.acknowledged
                        || binding.released && !record.revoked
                        || !binding.activated && !record.revoked
                        || !record.revoked && record.epoch.get() < 2
                        || binding.binding.identity.name != record.name
                        || binding.binding.identity.created_at_unix_ms != record.generation.get()
                        || record.native().as_ref() != Some(&binding.binding.identity)
                        || record.lifetime != (Lifetime::Persistent {})
                })
                || record.recipe.validate().is_err()
                || record.effective_recipe.validate().is_err()
                || record.lifetime.validate().is_err()
                || matches!(record.phase, Phase::Running) && record.identity.is_none()
                || record.artifacts_retired && !matches!(record.phase, Phase::Absent)
                || record.generation.get() > state.last_generation
            {
                return Err(ControlError::Storage);
            }
            if state
                .sessions
                .values()
                .map(|record| (record.root(&self.root), &record.name))
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != state.sessions.len()
                || state
                    .sessions
                    .values()
                    .map(|record| (record.root(&self.root), record.generation.get()))
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != state.sessions.len()
            {
                return Err(ControlError::Storage);
            }
        }
        if state.operations.values().any(|operation| {
            operation.intent.len() != 64
                || !operation
                    .intent
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
                || operation.generation.get() > state.last_generation
                || state
                    .sessions
                    .get(&operation.logical)
                    .is_none_or(|record| record.owner != operation.owner)
                || operation.recreated && !operation.created
        }) {
            return Err(ControlError::Storage);
        }
        let tx = Transaction {
            directory,
            _lock: lock,
            state,
        };
        if initialized {
            tx.save()?;
        }
        Ok(tx)
    }

    fn admission<G>(
        record: Option<&Record>,
        owner: &PrincipalId,
        effect: Effect,
        admit: &mut impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<NativeAdmission<G>, ControlError> {
        if record.is_some_and(|record| &record.owner != owner) {
            return Err(ControlError::Denied);
        }
        let source = record
            .and_then(|record| record.binding.as_ref())
            .map(|imported| Self::binding_source(&imported.binding))
            .transpose()?;
        let guard = Self::policy_admission(record, owner, effect, admit)?;
        Ok(NativeAdmission {
            _policy: guard,
            _source: source,
        })
    }

    fn policy_admission<G>(
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
            issuance_epoch: None,
        };
        let guard = admit(&state)?;
        if record.is_some_and(|record| record.revoked) {
            return Err(ControlError::Revoked);
        }
        Ok(guard)
    }

    fn binding_source(
        binding: &crate::native_binding::Binding,
    ) -> Result<crate::native_binding::SourceFence, ControlError> {
        match binding.source_fence() {
            Ok(guard) => Ok(guard),
            Err(error) if crate::lock_is_contended(&error) => Err(ControlError::Busy),
            // Private sources have no legacy consent receipt whose loss means
            // revocation. Failed proof stays unavailable; only exact protected
            // reduction may revoke/release its retained grant.
            Err(_) if !binding.source_authority.is_legacy() => Err(ControlError::Unknown),
            Err(_) => {
                let generation = GenerationId::new(binding.identity.created_at_unix_ms)
                    .map_err(|_| ControlError::Invalid)?;
                let guard = LeaseGuard::acquire(&binding.runtime_root, generation)
                    .map_err(storage_error)?;
                let mut lease = guard.read().map_err(storage_error)?;
                if lease.name != binding.identity.name
                    || lease.native_pid != Some(binding.identity.pid)
                    || lease.grant.logical != binding.logical
                    || lease.grant.owner != binding.principal
                {
                    return Err(ControlError::Unknown);
                }
                if !lease.remote_revoked || lease.grant.revision.get() == 1 {
                    lease.remote_revoked = true;
                    lease.pending = None;
                    lease.grant.remote_access = false;
                    lease.grant.state = LogicalState::Revoked;
                    lease.grant.revision = lease
                        .grant
                        .revision
                        .next()
                        .map_err(|_| ControlError::Storage)?;
                    guard.replace(&mut lease).map_err(storage_error)?;
                }
                Err(ControlError::Revoked)
            }
        }
    }

    fn reconcile(&self, record: &mut Record) -> Result<(), ControlError> {
        if record.revoked || record.artifacts_retired {
            return Ok(());
        }
        let guard = LeaseGuard::acquire(record.root(&self.root), record.generation)
            .map_err(storage_error)?;
        let lease = guard.read().map_err(|_| ControlError::Unknown)?;
        if !record.lease_matches(&lease) {
            return Err(ControlError::Unknown);
        }
        if record
            .binding
            .as_ref()
            .is_some_and(|binding| binding.activated)
            && !record.revoked
            && lease.remote_revoked
            && lease.grant.revision.get() == 1
        {
            return Err(ControlError::Busy);
        }
        if lease.remote_revoked {
            record.revoked = true;
            if lease.grant.revision.get() > record.epoch.get() {
                record.epoch = lease.grant.revision;
            }
            return Ok(());
        }
        drop(guard);
        let mut next = record.clone();
        self.reconcile_native(&mut next)?;
        self.sync_grant(&mut next)?;
        *record = next;
        Ok(())
    }

    fn sync_grant(&self, record: &mut Record) -> Result<(), ControlError> {
        let guard = LeaseGuard::acquire(record.root(&self.root), record.generation)
            .map_err(storage_error)?;
        let mut lease = guard.read().map_err(storage_error)?;
        if !record.lease_matches(&lease) {
            return Err(ControlError::Unknown);
        }
        if lease.remote_revoked {
            record.revoked = true;
        }
        if !same_grant(&lease.grant, &record.grant()) {
            if lease.grant.revision.get() >= record.epoch.get() {
                record.epoch = lease
                    .grant
                    .revision
                    .next()
                    .map_err(|_| ControlError::Storage)?;
            }
            lease.grant = record.grant();
            guard.replace(&mut lease).map_err(storage_error)?;
        }
        Ok(())
    }

    fn reconcile_native(&self, record: &mut Record) -> Result<(), ControlError> {
        if matches!(record.phase, Phase::Absent) {
            return Ok(());
        }
        let previous = record.phase.clone();
        if let Some(binding) = &record.binding {
            if binding.released {
                return Err(ControlError::Revoked);
            }
            if !detached::host_generation_state(
                record.root(&self.root),
                &binding.binding.identity,
                &record.fingerprint,
            )
            .map_err(|_| ControlError::Unknown)?
            {
                record.phase = Phase::Absent;
            }
            if record.phase != previous {
                record.epoch = record.epoch.next().map_err(|_| ControlError::Storage)?;
            }
            return Ok(());
        }
        // A pending reservation can only adopt its exact journaled incarnation.
        // It must never enter the spawning path again after restart/lost reply.
        if matches!(record.phase, Phase::Launching | Phase::Ending) && record.identity.is_none() {
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
                if matches!(record.phase, Phase::Launching) {
                    record.phase = Phase::Running;
                }
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
        admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<ProtectedLookup, ControlError> {
        let mut tx = self.transaction()?;
        let mut admit = scoped_issuance(tx.state.issuance_epoch, admit);
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
            Some(LogicalState::Unknown | LogicalState::Ended(_)) => Lookup::Unknown,
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
        admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<ProtectedInventory, ControlError> {
        let mut tx = self.transaction()?;
        let mut admit = scoped_issuance(tx.state.issuance_epoch, admit);
        Self::admission(None, owner, Effect::Lookup, &mut admit)?;
        let mut uncertain = std::collections::BTreeSet::new();
        for record in tx
            .state
            .sessions
            .values_mut()
            .filter(|record| &record.owner == owner && !record.revoked)
        {
            if self.reconcile(record).is_err()
                || !matches!(record.phase, Phase::Running | Phase::Absent)
            {
                uncertain.insert(record.logical.clone());
            }
        }
        tx.save()?;
        let mut result = ProtectedInventory {
            sessions: Vec::new(),
            unknown: Vec::new(),
        };
        for record in tx.state.sessions.values().filter(|record| {
            &record.owner == owner && !record.revoked && !matches!(record.phase, Phase::Absent)
        }) {
            let (_source, source_uncertain) = match record
                .binding
                .as_ref()
                .map(|imported| Self::binding_source(&imported.binding))
                .transpose()
            {
                Ok(source) => (source, false),
                Err(ControlError::Unknown | ControlError::Busy)
                    if record
                        .binding
                        .as_ref()
                        .is_some_and(|imported| !imported.binding.source_authority.is_legacy()) =>
                {
                    (None, true)
                }
                Err(error) => return Err(error),
            };
            // Source uncertainty may hide this summary, never owner-policy denial.
            let _admission =
                Self::policy_admission(Some(record), owner, Effect::Lookup, &mut admit)?;
            let summary = if source_uncertain || uncertain.contains(&record.logical) {
                None
            } else {
                self.summary(record).ok()
            };
            if let Some(summary) = summary {
                result.sessions.push(ProtectedSession {
                    grant: record.grant(),
                    summary,
                });
            } else {
                let mut grant = record.grant();
                grant.state = LogicalState::Unknown;
                result.unknown.push(ProtectedUnknown {
                    grant,
                    target: record.target(),
                });
            }
        }
        let _publication = Self::admission(None, owner, Effect::Lookup, &mut admit)?;
        Ok(result)
    }

    /// Capture once for a new operation, never to freshen an uncertain retry.
    pub fn issuance_epoch<G>(
        &self,
        owner: &PrincipalId,
        mut admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<Epoch, ControlError> {
        let mut tx = self.transaction()?;
        {
            let mut scoped = scoped_issuance(tx.state.issuance_epoch, &mut admit);
            let _admission = Self::admission(None, owner, Effect::Lookup, &mut scoped)?;
        }
        if tx.state.operations.len() >= MAX_IDEMPOTENCY_RECORDS {
            tx.state.issuance_epoch = tx
                .state
                .issuance_epoch
                .next()
                .map_err(|_| ControlError::Storage)?;
            tx.state.operations.clear();
        }
        let mut scoped = scoped_issuance(tx.state.issuance_epoch, &mut admit);
        let _publication = Self::admission(None, owner, Effect::Lookup, &mut scoped)?;
        // Even a fresh empty namespace must become durable before publication.
        tx.save()?;
        Ok(tx.state.issuance_epoch)
    }

    /// Explicit Connect/repair only, not a background reconnect primitive.
    /// `create_only` refuses an already existing logical identity, even if dead.
    pub fn ensure<G>(
        &self,
        owner: &PrincipalId,
        issued: &IssuedOperation,
        creation: &Creation,
        create_only: bool,
        cancelled: &AtomicBool,
        admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<EnsureResult, ControlError> {
        if self.helper.as_os_str().is_empty() {
            return Err(ControlError::Denied);
        }
        creation.validate().map_err(|_| ControlError::Invalid)?;
        let mut tx = self.transaction()?;
        let mut admit = scoped_issuance(tx.state.issuance_epoch, admit);
        let logical = &creation.logical;
        if tx
            .state
            .sessions
            .get(logical)
            .is_some_and(|record| record.binding.is_some())
        {
            return Err(ControlError::Denied);
        }
        Self::admission(
            tx.state.sessions.get(logical),
            owner,
            Effect::Resume,
            &mut admit,
        )?;
        if issued.epoch != tx.state.issuance_epoch {
            return Err(ControlError::Stale);
        }
        let operation = &issued.id;
        let intent = Operation::intent(owner, creation, create_only)?;
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
            if &previous.owner != owner || previous.intent != intent {
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
            let _admission = Self::admission(
                tx.state.sessions.get(logical),
                owner,
                Effect::Resume,
                &mut admit,
            )?;
            tx.rotate_issuance()?;
            return Err(ControlError::Stale);
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
                        logical: logical.clone(),
                        intent,
                        generation,
                        created: false,
                        recreated: false,
                    },
                );
                tx.save()?;
                return Ok(result);
            }
        } else if tx.state.sessions.len() >= MAX_SESSIONS {
            let retired = tx
                .state
                .sessions
                .values()
                .find(|record| {
                    &record.owner == owner
                        && !record.revoked
                        && matches!(record.phase, Phase::Absent)
                })
                .map(|record| record.logical.clone());
            if let Some(logical) = retired {
                let _admission = Self::admission(None, owner, Effect::Create, &mut admit)?;
                self.retire_artifacts(&mut tx, &logical)?;
                tx.state.sessions.remove(&logical);
                tx.rotate_issuance()?;
                return Err(ControlError::Stale);
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
        if tx.state.sessions.contains_key(logical) {
            self.retire_artifacts(&mut tx, logical)?;
        }
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
        let name = match tx.state.sessions.get(logical) {
            Some(record) => record.name.clone(),
            None => (0..MAX_SESSIONS)
                .map(native_slot)
                .find(|name| {
                    tx.state
                        .sessions
                        .values()
                        .all(|record| &record.name != name)
                })
                .ok_or(ControlError::Busy)?,
        };
        let record = Record {
            binding: None,
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
            name,
            launch: operation.clone(),
            generation,
            fingerprint,
            phase: Phase::Launching,
            identity: None,
            artifacts_retired: false,
        };
        tx.state.sessions.insert(logical.clone(), record);
        tx.state.last_generation = generation.get();
        tx.state.operations.insert(
            operation.clone(),
            Operation {
                owner: owner.clone(),
                logical: logical.clone(),
                intent,
                generation,
                created: true,
                recreated,
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
            grant: record.grant(),
            writer_epoch: Epoch::new(1).expect("nonzero epoch"),
            native_pid: None,
        };
        LeaseGuard::reserve(&self.root, generation)
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
        self.sync_grant(record)
            .map_err(|_| ControlError::CommittedButNotPublished)?;
        tx.save()
            .map_err(|_| ControlError::CommittedButNotPublished)?;
        let record = &tx.state.sessions[logical];
        let _publication = Self::admission(Some(record), owner, Effect::Resume, &mut admit)
            .map_err(|_| ControlError::CommittedButNotPublished)?;
        self.result(record, true, recreated, cancelled)
    }

    fn summary(&self, record: &Record) -> Result<SessionSummary, ControlError> {
        let identity = record.native().ok_or(ControlError::Unknown)?;
        let attached = detached::host_generation_attached(record.root(&self.root), &identity)
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
            initial_lease_authority: lease
                .temporary
                .as_ref()
                .filter(|_| {
                    lease.writer_epoch.get() == 1
                        && lease.last_attachment.is_none()
                        && lease.pending.is_none()
                        && !lease.connected
                })
                .map(|binding| InitialLeaseAuthority {
                    root: self.root.clone(),
                    target: record.target(),
                    grant: record.grant(),
                    binding: binding.clone(),
                }),
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
    ) -> Result<(&'a mut Record, NativeAdmission<G>), ControlError> {
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
        if effect == Effect::End && record.binding.is_some() {
            return Err(ControlError::Denied);
        }
        self.reconcile(record)?;
        let observed_end = effect == Effect::End
            && record.binding.is_none()
            && matches!(record.phase, Phase::Absent)
            && record.identity.is_some()
            && !record.revoked
            && !record.artifacts_retired;
        if !matches!(record.phase, Phase::Running) && !observed_end {
            return Err(ControlError::Unknown);
        }
        tx.save()?;
        let record = tx
            .state
            .sessions
            .get_mut(&target.logical)
            .ok_or(ControlError::Storage)?;
        let _observation_lease = if observed_end {
            let guard =
                LeaseGuard::acquire(&self.root, record.generation).map_err(storage_error)?;
            let lease = guard.read().map_err(|_| ControlError::Unknown)?;
            if lease.remote_revoked {
                return Err(ControlError::Revoked);
            }
            if !record.lease_matches(&lease) || !same_grant(&lease.grant, &record.grant()) {
                return Err(ControlError::Unknown);
            }
            Some(guard)
        } else {
            None
        };
        let guard = Self::admission(Some(record), owner, effect, &mut |state| {
            if observed_end {
                let mut grant = state.grant.clone().ok_or(ControlError::Unknown)?;
                grant.state = LogicalState::Ended(target.generation);
                admit(&AdmissionState {
                    grant: Some(grant),
                    grant_epoch: state.grant_epoch,
                    effect: state.effect,
                    issuance_epoch: state.issuance_epoch,
                })
            } else {
                admit(state)
            }
        })?;
        Ok((record, guard))
    }

    /// One NONBLOCKING action under per-generation writer and fresh policy
    /// fences. No global session journal maintenance or native IPC occurs here.
    pub fn with_attachment_authorized<G, T>(
        &self,
        authority: &AttachmentAuthority,
        admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
        action: impl FnOnce() -> Result<T, ControlError>,
    ) -> Result<T, ControlError> {
        self.with_authorized_lease(authority, Effect::Attach, admit, |_, lease| {
            Self::committed(authority, &lease)?;
            action()
        })
    }

    /// Authorizes pending recovery retrieval/adoption, NOT Attached success.
    /// The ticket must still name the live pending transfer, or that exact
    /// transfer must already have committed with the expected binding.
    pub fn with_pending_attachment_authorized<G, T>(
        &self,
        authority: &AttachmentAuthority,
        admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
        action: impl FnOnce() -> Result<T, ControlError>,
    ) -> Result<T, ControlError> {
        self.with_authorized_lease(authority, Effect::Attach, admit, |_, lease| {
            let committed = Self::committed(authority, &lease).is_ok();
            let pending = lease.pending.as_ref().is_some_and(|pending| {
                pending.ticket == authority.ticket
                    && pending.deadline_ms > now_ms()
                    && lease.writer_epoch.next().ok() == Some(authority.writer_epoch)
            });
            if !committed && !pending {
                return Err(ControlError::Stale);
            }
            action()
        })
    }

    fn with_authorized_lease<G, T>(
        &self,
        authority: &AttachmentAuthority,
        effect: Effect,
        admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
        action: impl FnOnce(&LeaseGuard, DaemonLease) -> Result<T, ControlError>,
    ) -> Result<T, ControlError> {
        self.with_generation_authorized(authority, effect, false, admit, action)
    }

    fn with_generation_authorized<G, T>(
        &self,
        authority: &AttachmentAuthority,
        effect: Effect,
        allow_stopped_receipt: bool,
        mut admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
        action: impl FnOnce(&LeaseGuard, DaemonLease) -> Result<T, ControlError>,
    ) -> Result<T, ControlError> {
        if authority.root != self.root {
            return Err(ControlError::Denied);
        }
        if let Some(binding) = &authority.binding {
            let tx = self.transaction()?;
            let record = tx
                .state
                .sessions
                .get(&binding.logical)
                .ok_or(ControlError::Unknown)?;
            if tx.state.namespace.as_ref() != Some(&binding.namespace)
                || record.revoked
                || record.binding.as_ref().is_none_or(|current| {
                    current.binding != *binding || !current.activated || current.released
                })
            {
                return Err(ControlError::Revoked);
            }
            let directory =
                storage::private_directory(&binding.runtime_root, false).map_err(storage_error)?;
            let bytes = storage::read_private(
                &directory,
                Path::new(&format!(
                    "native-binding-{}-{}.json",
                    binding.identity.pid, binding.identity.created_at_unix_ms
                )),
                16 * 1024,
            )
            .map_err(storage_error)?;
            let receipt: crate::native_binding::Receipt =
                serde_json::from_slice(&bytes).map_err(|_| ControlError::Storage)?;
            if receipt.version != 1
                || receipt.binding != *binding
                || receipt.phase != crate::native_binding::BindingPhase::Active
            {
                return Err(ControlError::Revoked);
            }
        }
        let _source = authority
            .binding
            .as_ref()
            .map(Self::binding_source)
            .transpose()?;
        let guard = LeaseGuard::acquire(&authority.native_root, authority.target.generation)
            .map_err(|error| attachment_storage_error("authority-acquire", error))?;
        let lease = guard
            .read()
            .map_err(|error| attachment_storage_error("authority-read", error))?;
        if lease.remote_revoked {
            return Err(ControlError::Revoked);
        }
        if !same_grant(&lease.grant, &authority.grant)
            || lease.name != authority.identity.name
            || lease.native_pid != Some(authority.identity.pid)
        {
            return Err(ControlError::Stale);
        }
        let _admission = admit(&AdmissionState {
            grant: Some(lease.grant.clone()),
            grant_epoch: Some(lease.grant.revision),
            effect,
            issuance_epoch: None,
        })?;
        if lease.expired() && !(allow_stopped_receipt && lease.stopped) {
            return Err(ControlError::Ended);
        }
        if !(allow_stopped_receipt && lease.stopped)
            && !detached::host_generation_lease_live(&authority.native_root, &authority.identity)
                .map_err(|_| ControlError::Unknown)?
        {
            return Err(ControlError::Ended);
        }
        action(&guard, lease)
    }

    fn committed(authority: &AttachmentAuthority, lease: &DaemonLease) -> Result<(), ControlError> {
        if lease.writer_epoch != authority.writer_epoch
            || lease.last_attachment.as_ref() != Some(&authority.ticket)
            || lease.temporary != authority.expected
        {
            return Err(ControlError::Stale);
        }
        Ok(())
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
        admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<PendingAttachment, ControlError> {
        let mut tx = self.transaction()?;
        let mut admit = scoped_issuance(tx.state.issuance_epoch, admit);
        let (record, admission) = self.exact(&mut tx, owner, target, Effect::Attach, &mut admit)?;
        let guard = LeaseGuard::acquire(record.root(&self.root), record.generation)
            .map_err(storage_error)?;
        let mut lease = guard.read().map_err(storage_error)?;
        if lease.expired() {
            return Err(ControlError::Ended);
        }

        #[cfg(any(test, feature = "test-support"))]
        let diagnose = attach_failure_diagnostics_enabled(
            lease.temporary.is_some(),
            takeover,
            lease.last_attachment.is_some(),
        );
        if !record.lease_matches(&lease) || !same_grant(&lease.grant, &record.grant()) {
            return Err(ControlError::Unknown);
        }
        if lease.remote_revoked {
            return Err(ControlError::Revoked);
        }
        if !takeover
            && record.binding.is_none()
            && lease.temporary.is_none()
            && lease.connected
            && lease.last_attachment.is_some()
        {
            #[cfg(any(test, feature = "test-support"))]
            AttachFailureDiagnostic::ConnectedPersistentRefusal.report(diagnose);
            return Err(ControlError::Busy);
        }
        let transfer = lease
            .temporary
            .as_ref()
            .is_some_and(|lease| &lease.attachment != attachment);
        if transfer && !takeover {
            return Err(ControlError::Busy);
        }
        let ticket = Some(OperationId::new());
        let mut reserved_lease = lease.temporary.clone();
        if lease
            .pending
            .as_ref()
            .is_some_and(|pending| pending.deadline_ms > now_ms())
        {
            return Err(ControlError::Busy);
        }
        lease.pending = Some(PendingLeaseAttachment {
            ticket: ticket.clone().ok_or(ControlError::Storage)?,
            writer_epoch: lease.writer_epoch,
            previous: lease.temporary.clone(),
            attachment: attachment.clone(),
            deadline_ms: now_ms() + 15_000,
        });
        if let Some(binding) = reserved_lease.as_mut() {
            binding.epoch = binding.epoch.next().map_err(|_| ControlError::Storage)?;
            binding.attachment = attachment.clone();
        }
        let publication = guard.replace(&mut lease);
        #[cfg(any(test, feature = "test-support"))]
        if let Err(error) = &publication {
            AttachFailureDiagnostic::ReservePublish(error).report(diagnose);
        }
        publication.map_err(|_| ControlError::CommittedButNotPublished)?;
        let identity = record.native().ok_or(ControlError::Unknown)?;
        let authority = AttachmentAuthority {
            root: self.root.clone(),
            native_root: record.root(&self.root).into(),
            binding: record
                .binding
                .as_ref()
                .map(|imported| imported.binding.clone()),
            target: record.target(),
            identity: identity.clone(),
            grant: record.grant(),
            writer_epoch: lease
                .writer_epoch
                .next()
                .map_err(|_| ControlError::Storage)?,
            ticket: ticket.clone().ok_or(ControlError::Storage)?,
            expected: reserved_lease.clone(),
        };
        drop(guard);
        drop(admission);
        let connected = match &ticket {
            Some(ticket) => PersistentSession::resume_host_reserved_in(
                &identity,
                record.root(&self.root),
                takeover,
                maximum_recovery_bytes,
                ticket,
                notifier,
            ),
            None => PersistentSession::resume_remote_in(
                &identity,
                record.root(&self.root),
                takeover,
                maximum_recovery_bytes,
                notifier,
            ),
        };
        let session = match connected {
            Ok(session) => session,
            Err(error) => {
                #[cfg(any(test, feature = "test-support"))]
                AttachFailureDiagnostic::NativeConnect(&error).report(diagnose);
                #[cfg(not(any(test, feature = "test-support")))]
                let _ = error;
                if let Some(ticket) = &ticket {
                    Self::unwind_attachment(
                        record.root(&self.root),
                        record.generation,
                        ticket,
                        #[cfg(any(test, feature = "test-support"))]
                        diagnose,
                    )?;
                }
                return Err(ControlError::Busy);
            }
        };
        let publication = Self::admission(Some(record), owner, Effect::Attach, &mut admit);
        #[cfg(any(test, feature = "test-support"))]
        if let Err(error) = &publication {
            AttachFailureDiagnostic::PublicationReadmit(*error).report(diagnose);
        }
        let _publication = publication.map_err(|_| ControlError::CommittedButNotPublished)?;
        Ok(PendingAttachment {
            session,
            ticket,
            reserved_lease,
            authority,
        })
    }

    fn unwind_attachment(
        root: &Path,
        generation: GenerationId,
        ticket: &OperationId,
        #[cfg(any(test, feature = "test-support"))] diagnose: bool,
    ) -> Result<(), ControlError> {
        let acquired = LeaseGuard::acquire(root, generation);
        #[cfg(any(test, feature = "test-support"))]
        if let Err(error) = &acquired {
            AttachFailureDiagnostic::UnwindAcquire(error).report(diagnose);
        }
        let guard = acquired.map_err(|_| ControlError::CommittedButNotPublished)?;
        let read = guard.read();
        #[cfg(any(test, feature = "test-support"))]
        if let Err(error) = &read {
            AttachFailureDiagnostic::UnwindRead(error).report(diagnose);
        }
        let mut lease = read.map_err(|_| ControlError::CommittedButNotPublished)?;
        if lease.last_attachment.as_ref() == Some(ticket) {
            #[cfg(any(test, feature = "test-support"))]
            AttachFailureDiagnostic::UnwindCommitted.report(diagnose);
            return Err(ControlError::CommittedButNotPublished);
        }
        if lease
            .pending
            .as_ref()
            .is_some_and(|pending| &pending.ticket == ticket)
        {
            lease.pending = None;
            let publication = guard.replace(&mut lease);
            #[cfg(any(test, feature = "test-support"))]
            if let Err(error) = &publication {
                AttachFailureDiagnostic::UnwindPublish(error).report(diagnose);
            }
            publication.map_err(|_| ControlError::CommittedButNotPublished)?;
        }
        Ok(())
    }

    /// Poll after the frontend has adopted the deferred recovery snapshot.
    /// Busy means not committed, not successful ownership or readiness.
    pub fn attachment_outcome<G>(
        &self,
        authority: &AttachmentAuthority,
        admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<Option<TemporaryLease>, ControlError> {
        self.with_authorized_lease(authority, Effect::Attach, admit, |_, lease| {
            if lease
                .pending
                .as_ref()
                .is_some_and(|pending| pending.ticket == authority.ticket)
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
            Self::committed(authority, &lease)?;
            Ok(authority.expected.clone())
        })
    }

    /// Renew at intervals shorter than 60s while the owning channel is alive.
    /// A heartbeat is lease maintenance, never shell input or a reconnect.
    pub fn maintain_lease<G>(
        &self,
        authority: &AttachmentAuthority,
        disconnect: Option<DetachKind>,
        admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<Option<DetachmentReceipt>, ControlError> {
        self.with_generation_authorized(
            authority,
            Effect::Lease,
            disconnect == Some(DetachKind::Intentional),
            admit,
            |guard, mut lease| {
                Self::committed(authority, &lease)?;
                if lease.temporary.is_none() {
                    let Some(kind) = disconnect else {
                        return Ok(None);
                    };
                    lease.writer_epoch = lease
                        .writer_epoch
                        .next()
                        .map_err(|_| ControlError::Storage)?;
                    lease.pending = None;
                    lease.connected = false;
                    guard.replace(&mut lease).map_err(storage_error)?;
                    let mut receipt_authority = authority.clone();
                    receipt_authority.writer_epoch = lease.writer_epoch;
                    return Ok(Some(DetachmentReceipt {
                        authority: receipt_authority,
                        revision: lease.revision,
                        kind,
                    }));
                }
                if lease.stopped {
                    return Ok(Some(DetachmentReceipt {
                        authority: authority.clone(),
                        revision: lease.revision,
                        kind: DetachKind::Intentional,
                    }));
                }
                match disconnect {
                    Some(DetachKind::Intentional) => {
                        lease.stopped = true;
                        lease.connected = false;
                    }
                    // A missing bridge heartbeat already starts the same bounded grace.
                    Some(DetachKind::TransportLost) if lease.connected => {
                        lease.connected = false;
                        lease.deadline_ms = Some(now_ms() + 60_000);
                    }
                    Some(DetachKind::TransportLost) => {}
                    None if lease.connected => lease.deadline_ms = Some(now_ms() + 60_000),
                    None => return Err(ControlError::Stale),
                }
                guard.replace(&mut lease).map_err(storage_error)?;
                Ok(disconnect.map(|kind| DetachmentReceipt {
                    authority: authority.clone(),
                    revision: lease.revision,
                    kind,
                }))
            },
        )
    }

    /// Receipt-only Detached publication. This does not authorize terminal
    /// actions, native IPC, renewal, or a claim that the daemon has exited.
    pub fn with_detachment_receipt_authorized<G, T>(
        &self,
        receipt: &DetachmentReceipt,
        admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
        publish: impl FnOnce() -> Result<T, ControlError>,
    ) -> Result<T, ControlError> {
        self.with_generation_authorized(
            &receipt.authority,
            Effect::Lease,
            receipt.kind == DetachKind::Intentional,
            admit,
            |_, lease| {
                Self::committed(&receipt.authority, &lease)?;
                if lease.revision != receipt.revision {
                    return Err(ControlError::Stale);
                }
                publish()
            },
        )
    }

    /// Dropping a pending frontend does not imply its adoption failed. Cancel
    /// under the same lease lock as daemon commit and return the actual outcome.
    pub fn cancel_attachment<G>(
        &self,
        authority: &AttachmentAuthority,
        admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<AttachmentCancellation, ControlError> {
        self.with_authorized_lease(authority, Effect::Attach, admit, |guard, mut lease| {
            if Self::committed(authority, &lease).is_ok() {
                return Ok(AttachmentCancellation::Committed(lease.temporary));
            }
            if lease.pending.as_ref().is_some_and(|pending| {
                pending.ticket == authority.ticket
                    && lease.writer_epoch.next().ok() == Some(authority.writer_epoch)
            }) {
                lease.pending = None;
                guard.replace(&mut lease).map_err(storage_error)?;
                return Ok(AttachmentCancellation::CancelledBeforeCommit);
            }
            Err(ControlError::Stale)
        })
    }

    /// Requests exact-generation termination or freshly authorizes confirmation
    /// that its previously bound native process is authoritatively gone.
    pub fn end<G>(
        &self,
        owner: &PrincipalId,
        target: &Target,
        admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<EndOutcome, ControlError> {
        let mut tx = self.transaction()?;
        let mut admit = scoped_issuance(tx.state.issuance_epoch, admit);
        let (record, _admission) = self.exact(&mut tx, owner, target, Effect::End, &mut admit)?;
        if record.binding.is_some() {
            return Err(ControlError::Denied);
        }
        if matches!(record.phase, Phase::Absent) {
            return Ok(EndOutcome::ObservedEnded);
        }
        let guard = LeaseGuard::acquire(record.root(&self.root), record.generation)
            .map_err(storage_error)?;
        let mut lease = guard.read().map_err(storage_error)?;
        if !record.lease_matches(&lease) {
            return Err(ControlError::Unknown);
        }
        record.revoked |= lease.remote_revoked;
        record.phase = Phase::Ending;
        let epoch = if record.epoch.get() > lease.grant.revision.get() {
            record.epoch
        } else {
            lease.grant.revision
        };
        record.epoch = epoch.next().map_err(|_| ControlError::Storage)?;
        lease.stopped = true;
        lease.grant = record.grant();
        guard.replace(&mut lease).map_err(storage_error)?;
        tx.save()?;
        Ok(EndOutcome::StopRequested)
    }

    /// Intentional cancellation after creation committed but before adoption.
    /// A pending or committed transfer makes this original authority stale.
    pub fn cancel_initial_lease<G>(
        &self,
        authority: &InitialLeaseAuthority,
        admit: impl FnMut(&AdmissionState) -> Result<G, ControlError>,
    ) -> Result<EndOutcome, ControlError> {
        if authority.root != self.root {
            return Err(ControlError::Denied);
        }
        let mut tx = self.transaction()?;
        let mut admit = scoped_issuance(tx.state.issuance_epoch, admit);
        let (record, _admission) = self.exact(
            &mut tx,
            &authority.grant.owner,
            &authority.target,
            Effect::Lease,
            &mut admit,
        )?;
        let guard = LeaseGuard::acquire(&self.root, record.generation).map_err(storage_error)?;
        let mut lease = guard.read().map_err(storage_error)?;
        if !same_grant(&record.grant(), &authority.grant)
            || lease.writer_epoch.get() != 1
            || lease.connected
            || lease.pending.is_some()
            || lease.last_attachment.is_some()
            || lease.temporary.as_ref() != Some(&authority.binding)
        {
            return Err(ControlError::Stale);
        }
        if lease.expired() {
            return Err(ControlError::Ended);
        }
        record.phase = Phase::Ending;
        record.epoch = record.epoch.next().map_err(|_| ControlError::Storage)?;
        lease.stopped = true;
        lease.grant = record.grant();
        guard.replace(&mut lease).map_err(storage_error)?;
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
        if record.artifacts_retired {
            record.epoch = record.epoch.next().map_err(|_| ControlError::Storage)?;
            record.revoked = true;
            return tx.save();
        }
        let guard = LeaseGuard::acquire(record.root(&self.root), record.generation)
            .map_err(storage_error)?;
        let mut lease = guard.read().map_err(storage_error)?;
        lease.pending = None;
        lease.remote_revoked = true;
        let epoch = if record.epoch.get() > lease.grant.revision.get() {
            record.epoch
        } else {
            lease.grant.revision
        };
        record.epoch = epoch.next().map_err(|_| ControlError::Storage)?;
        record.revoked = true;
        lease.grant = record.grant();
        guard.replace(&mut lease).map_err(storage_error)?;
        tx.save()
    }

    fn retire_artifacts(
        &self,
        tx: &mut Transaction,
        logical: &LogicalSessionId,
    ) -> Result<(), ControlError> {
        let record = tx.state.sessions.get(logical).ok_or(ControlError::Denied)?;
        if record.binding.is_some() {
            return Err(ControlError::Denied);
        }
        let _start = detached::lock_native_start_in(&self.root, &record.name)
            .map_err(|_| ControlError::Busy)?;
        let mut native_pid = record.identity.as_ref().map(|identity| identity.pid);
        let mut sealed = None;
        if !record.artifacts_retired {
            let guard =
                LeaseGuard::reserve(&self.root, record.generation).map_err(storage_error)?;
            let mut lease = match guard.read() {
                Ok(lease) => lease,
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound && record.identity.is_none() =>
                {
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
                        remote_revoked: record.revoked,
                        pending: None,
                        last_attachment: None,
                        grant: record.grant(),
                        writer_epoch: Epoch::new(1).expect("nonzero epoch"),
                        native_pid: None,
                    };
                    guard.create(&lease).map_err(storage_error)?;
                    lease
                }
                Err(_) => return Err(ControlError::Unknown),
            };
            if !record.lease_matches(&lease) {
                return Err(ControlError::Unknown);
            }
            if lease.remote_revoked && !record.revoked {
                let record = tx
                    .state
                    .sessions
                    .get_mut(logical)
                    .ok_or(ControlError::Storage)?;
                record.revoked = true;
                record.epoch = lease.grant.revision;
                tx.save()?;
                return Err(ControlError::Revoked);
            }
            lease.stopped = true;
            lease.pending = None;
            native_pid = lease.native_pid.or(native_pid);
            guard.replace(&mut lease).map_err(storage_error)?;
            sealed = Some(guard);
        }
        let record = &tx.state.sessions[logical];
        let retirement = detached::prove_host_reservation_absence(
            &self.root,
            &record.name,
            record.generation.get(),
            record.native().as_ref(),
            native_pid,
            &record.fingerprint,
        )
        .map_err(|error| {
            if crate::lock_is_contended(&error) {
                ControlError::Busy
            } else {
                ControlError::Unknown
            }
        })?;
        let record = tx
            .state
            .sessions
            .get_mut(logical)
            .ok_or(ControlError::Storage)?;
        if !record.artifacts_retired {
            record.phase = Phase::Absent;
            record.epoch = record.epoch.next().map_err(|_| ControlError::Storage)?;
            record.identity = retirement.identity.as_ref().map(|identity| Identity {
                pid: identity.pid,
                endpoint: identity.endpoint.clone(),
            });
            record.artifacts_retired = true;
            tx.save()?;
        }
        let record = &tx.state.sessions[logical];
        detached::remove_host_generation_artifacts(
            &self.root,
            record.generation.get(),
            record.launch.as_str(),
            &retirement,
        )
        .map_err(|_| ControlError::Unknown)?;
        drop(sealed);
        Ok(())
    }

    /// LOCAL maintenance. Known absence may be reclaimed; uncertain creation
    /// first seals future helpers and requires positive native absence proof.
    /// Live revoked daemons remain counted and remotely inaccessible.
    /// Contention cannot mask foreign-owner denial when the protected checkpoint
    /// is readable. This read-only check never authorizes retirement.
    pub fn retire_local(&self, owner: &PrincipalId, target: &Target) -> Result<(), ControlError> {
        let mut tx = match self.transaction() {
            Err(ControlError::Busy) => {
                let directory =
                    storage::private_directory(&self.root, false).map_err(storage_error)?;
                let bytes = storage::read_private(&directory, Path::new(STATE_FILE), STATE_LIMIT)
                    .map_err(storage_error)?;
                let state: State =
                    serde_json::from_slice(&bytes).map_err(|_| ControlError::Storage)?;
                return Err(
                    if state
                        .sessions
                        .get(&target.logical)
                        .is_none_or(|record| &record.owner != owner)
                    {
                        ControlError::Denied
                    } else {
                        ControlError::Busy
                    },
                );
            }
            result => result?,
        };
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
        if record.revoked {
            return Err(ControlError::Revoked);
        }
        if !matches!(record.phase, Phase::Absent | Phase::Launching) {
            return Err(ControlError::Busy);
        }
        self.retire_artifacts(&mut tx, &target.logical)?;
        tx.state.sessions.remove(&target.logical);
        tx.rotate_issuance()
    }

    /// LOCAL observation only; never adopts a frontend or restores sharing.
    pub fn observe_local(
        &self,
        owner: &PrincipalId,
        target: &Target,
    ) -> Result<LocalObservation, ControlError> {
        let mut tx = self.transaction()?;
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
        let mut next = record.clone();
        self.reconcile_native(&mut next)?;
        if !next.artifacts_retired {
            self.sync_grant(&mut next)?;
        }
        *record = next;
        let unbound_end = matches!(record.phase, Phase::Ending) && record.identity.is_none();
        tx.save()?;
        if unbound_end {
            self.retire_artifacts(&mut tx, &target.logical)?;
        }
        let record = &tx.state.sessions[&target.logical];
        let state = match record.phase {
            Phase::Absent => Lookup::Absent,
            Phase::Running => Lookup::Running(target.clone()),
            Phase::Launching | Phase::Ending => Lookup::Unknown,
        };
        Ok(LocalObservation {
            target: record.target(),
            revision: record.epoch,
            revoked: record.revoked,
            state,
        })
    }

    /// LOCAL exact-generation stop, including an unshared owned daemon.
    pub fn end_local(
        &self,
        owner: &PrincipalId,
        target: &Target,
    ) -> Result<EndOutcome, ControlError> {
        let mut tx = self.transaction()?;
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
        if record.binding.is_some() {
            return Err(ControlError::Denied);
        }
        if matches!(record.phase, Phase::Absent) {
            return Err(ControlError::Ended);
        }
        let guard = LeaseGuard::acquire(&self.root, record.generation).map_err(storage_error)?;
        let mut lease = guard.read().map_err(storage_error)?;
        if !record.lease_matches(&lease) {
            return Err(ControlError::Unknown);
        }
        record.revoked |= lease.remote_revoked;
        record.phase = Phase::Ending;
        let epoch = if record.epoch.get() > lease.grant.revision.get() {
            record.epoch
        } else {
            lease.grant.revision
        };
        record.epoch = epoch.next().map_err(|_| ControlError::Storage)?;
        lease.stopped = true;
        lease.pending = None;
        lease.grant = record.grant();
        guard.replace(&mut lease).map_err(storage_error)?;
        tx.save()?;
        Ok(EndOutcome::StopRequested)
    }

    /// Explicit new LOCAL consent after exact authoritative end. This is the
    /// only path that can retire a revoked logical fence.
    pub fn reconfirm_local(
        &self,
        owner: &PrincipalId,
        target: &Target,
        revision: Epoch,
    ) -> Result<(), ControlError> {
        let mut tx = self.transaction()?;
        let record = tx
            .state
            .sessions
            .get(&target.logical)
            .ok_or(ControlError::Denied)?;
        if &record.owner != owner {
            return Err(ControlError::Denied);
        }
        if record.generation != target.generation || record.epoch != revision {
            return Err(ControlError::Stale);
        }
        if !record.revoked || !matches!(record.phase, Phase::Absent) {
            return Err(ControlError::Busy);
        }
        self.retire_artifacts(&mut tx, &target.logical)?;
        tx.state.sessions.remove(&target.logical);
        tx.rotate_issuance()
    }
}

fn native_slot(slot: usize) -> String {
    format!("hs-slot-{slot:02}")
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
