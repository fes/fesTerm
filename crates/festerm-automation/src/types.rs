use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

pub const MAX_GRANT_LIFETIME: Duration = Duration::from_secs(300);
pub const MAX_PAYLOAD_BYTES: u32 = 65_536;
pub const MAX_OBSERVATION_BYTES: u32 = 32_768;
pub const MAX_INPUT_BYTES: u32 = 16_384;
pub const MAX_SCOPE_TARGETS: usize = 32;
pub const MAX_CONTROLLER_REGISTRATION_BYTES: usize = 128;
pub const MAX_PENDING_APPROVALS: usize = 16;

macro_rules! reference_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            pub(crate) instance: [u8; 16],
            pub(crate) sequence: u64,
        }
    };
}

reference_id!(RunId);
reference_id!(RequestId);
reference_id!(ApprovalId);

/// Obtained only from the trusted application acceptor, never decoded from IPC.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ConnectionId {
    pub(crate) instance: [u8; 16],
    pub(crate) sequence: u64,
}

/// An application-owned reference to one connection's opaque registration.
/// It is not a target, a grant, or a supported wire identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ControllerRegistrationId {
    pub(crate) instance: [u8; 16],
    pub(crate) sequence: u64,
}

/// A reference to one exact native-selected association, not authority.
/// A later native rebind mints a different reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ControllerEnrollmentId {
    pub(crate) instance: [u8; 16],
    pub(crate) sequence: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetRef {
    pub(crate) instance: [u8; 16],
    pub(crate) session: u64,
    pub(crate) generation: u64,
}

impl TargetRef {
    pub fn application_instance(self) -> [u8; 16] {
        self.instance
    }

    pub fn session(self) -> u64 {
        self.session
    }

    pub fn generation(self) -> u64 {
        self.generation
    }
}

/// SHA-256 digest computed by the application over a frozen canonical artifact,
/// schema-validated canonical parameters, or immutable encoded input; never an
/// asserted worker or client hash.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct Digest(pub [u8; 32]);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectKind {
    Discovery,
    Observation,
    WorkflowStart,
    RunStatus,
    RunCancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Invocation {
    Direct {
        operation: DirectKind,
    },
    /// One exact direct-input action; no workflow or observation is implied.
    DirectInput {
        target: TargetRef,
        /// Application-computed SHA-256 digest of the immutable encoded input.
        input: Digest,
        /// Length of those same encoded bytes.
        bytes: u32,
    },
    Workflow {
        bundle: Digest,
        manifest: Digest,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationBinding {
    pub api_version: u32,
    pub invocation: Invocation,
    /// Digest of the schema-validated canonical invocation parameters.
    pub parameters: Digest,
}

/// Requested scopes are data, not authority. Presentation uses its own native
/// decision, not these scopes. Results return only to the same connection.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub discover: BTreeSet<TargetRef>,
    pub observe: BTreeSet<TargetRef>,
    pub control: BTreeSet<TargetRef>,
    pub invoke_workflows: bool,
    pub return_results: bool,
}

impl Scope {
    pub(crate) fn subset_of(&self, parent: &Self) -> bool {
        self.discover.is_subset(&parent.discover)
            && self.observe.is_subset(&parent.observe)
            && self.control.is_subset(&parent.control)
            && (!self.invoke_workflows || parent.invoke_workflows)
            && (!self.return_results || parent.return_results)
    }

    pub(crate) fn sources(&self) -> BTreeSet<TargetRef> {
        self.discover.union(&self.observe).copied().collect()
    }

    pub(crate) fn targets(&self) -> BTreeSet<TargetRef> {
        self.sources().union(&self.control).copied().collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantRequest {
    pub scope: Scope,
    pub bindings: BTreeSet<InvocationBinding>,
    /// Explicitly allows schema-validated direct reads within `scope`, with
    /// each invocation still bound to its own canonical parameter digest.
    /// Only Discovery and Observation are supported; effects and workflows
    /// always require exact bindings.
    #[serde(default)]
    pub direct_read_calls: BTreeSet<DirectKind>,
}

/// A separate proposal for discovery of exactly these target generations.
/// It carries no observation, control, workflow or result authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataGrantRequest {
    pub targets: BTreeSet<TargetRef>,
}

/// Native UI decision. Deliberately neither serializable nor deserializable.
/// Only trusted application code may construct this after showing exact scopes,
/// invocation bindings, parameter/input digests and the caller identity.
pub struct NativeApproval {
    pub(crate) request: GrantRequest,
    pub(crate) lifetime: Duration,
    pub(crate) handoffs: BTreeMap<TargetRef, BTreeSet<TargetRef>>,
    pub(crate) enrollment_metadata: BTreeSet<crate::NativeEnrollmentMetadataGrant>,
    pub(crate) enrollment_disclosures:
        BTreeMap<crate::NativeEnrollmentMetadataGrant, BTreeSet<TargetRef>>,
    pub(crate) enrollment_review: Option<crate::NativeOrdinaryEnrollmentReview>,
}

impl NativeApproval {
    pub fn new(request: GrantRequest, lifetime: Duration) -> Self {
        Self {
            request,
            lifetime,
            handoffs: BTreeMap::new(),
            enrollment_metadata: BTreeSet::new(),
            enrollment_disclosures: BTreeMap::new(),
            enrollment_review: None,
        }
    }

    /// Native consent to unknown remote editing/TUI state AND disclosure of all
    /// listed sources through this exact account's execution/network authority.
    /// An empty local input queue is not evidence for granting this consent.
    /// Recording consent does not cut a current owner or acquire a writer lease.
    pub fn accept_control_handoff(
        mut self,
        target: TargetRef,
        all_readable_sources: BTreeSet<TargetRef>,
    ) -> Self {
        self.handoffs.insert(target, all_readable_sources);
        self
    }

    /// Begin a fresh native cross-consent decision for one frozen ordinary
    /// approval. Existing decisions cannot be rebound to a different review.
    ///
    /// ```compile_fail
    /// use festerm_automation::{NativeApproval, NativeOrdinaryEnrollmentReview};
    /// fn rebind(old: NativeApproval, review: NativeOrdinaryEnrollmentReview) {
    ///     let _ = old.bind_enrollment_review(review);
    /// }
    /// ```
    pub fn new_for_enrollment(
        request: GrantRequest,
        lifetime: Duration,
        review: crate::NativeOrdinaryEnrollmentReview,
    ) -> Self {
        let mut approval = Self::new(request, lifetime);
        approval.enrollment_review = Some(review);
        approval
    }

    /// Control-side acknowledgement of one exact existing metadata grant.
    pub fn accept_enrollment_metadata(
        mut self,
        grant: crate::NativeEnrollmentMetadataGrant,
    ) -> Self {
        self.enrollment_metadata.insert(grant);
        self
    }

    /// Metadata-side acknowledgement of this proposed control sink, reviewed
    /// separately for the exact existing enrollment grant.
    pub fn accept_metadata_control_sink(
        mut self,
        grant: crate::NativeEnrollmentMetadataGrant,
        sink: TargetRef,
    ) -> Self {
        self.enrollment_disclosures
            .entry(grant)
            .or_default()
            .insert(sink);
        self
    }

    /// Whether this native decision contains exactly one accepted control
    /// target. The host still has to validate the entire pending proposal.
    pub fn is_single_control_handoff(&self, target: TargetRef) -> bool {
        self.request.scope.control.len() == 1
            && self.request.scope.control.contains(&target)
            && self.handoffs.contains_key(&target)
    }
}

/// A frozen candidate for native review, not an association or approval.
#[derive(Debug)]
pub struct ControllerEnrollmentProposal {
    pub(crate) connection: ConnectionId,
    pub(crate) registration: ControllerRegistrationId,
    pub(crate) target: TargetRef,
    pub(crate) revision: u64,
}

impl ControllerEnrollmentProposal {
    pub fn registration(&self) -> ControllerRegistrationId {
        self.registration
    }

    pub fn target(&self) -> TargetRef {
        self.target
    }
}

/// Native-only decision to associate the reviewed registration and exact target.
/// Trusted application code constructs this only after explicit user selection.
/// It is deliberately neither serializable nor deserializable.
///
/// ```compile_fail
/// use festerm_automation::NativeControllerEnrollment;
/// let _ = serde_json::from_str::<NativeControllerEnrollment>("{}");
/// ```
pub struct NativeControllerEnrollment {
    pub(crate) proposal: ControllerEnrollmentProposal,
}

impl NativeControllerEnrollment {
    pub fn new(proposal: ControllerEnrollmentProposal) -> Self {
        Self { proposal }
    }
}

/// Native-only decision for a metadata proposal. Never construct this from IPC.
///
/// ```compile_fail
/// use festerm_automation::NativeMetadataApproval;
/// let _ = serde_json::from_str::<NativeMetadataApproval>("{}");
/// ```
pub struct NativeMetadataApproval {
    pub(crate) request: MetadataGrantRequest,
    pub(crate) lifetime: Duration,
    pub(crate) control_disclosures: BTreeMap<TargetRef, BTreeSet<TargetRef>>,
}

impl NativeMetadataApproval {
    pub fn new(request: MetadataGrantRequest, lifetime: Duration) -> Self {
        Self {
            request,
            lifetime,
            control_disclosures: BTreeMap::new(),
        }
    }

    /// Explicitly review these metadata sources flowing through a currently
    /// authorized control target. This does not acquire or transfer a writer.
    pub fn accept_control_disclosure(
        mut self,
        target: TargetRef,
        metadata_sources: BTreeSet<TargetRef>,
    ) -> Self {
        self.control_disclosures.insert(target, metadata_sources);
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Budget {
    pub operations: u32,
    pub disclosed_bytes: u32,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            operations: 256,
            disclosed_bytes: 1_048_576,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunSpec {
    pub binding: InvocationBinding,
    pub scope: Scope,
    pub budget: Budget,
    pub deadline: Instant,
}

/// A direct discovery root routed only to the independently approved metadata
/// grant. The application computes `parameters` from the validated operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataRunSpec {
    pub parameters: Digest,
    pub targets: BTreeSet<TargetRef>,
    pub budget: Budget,
    pub deadline: Instant,
}

/// Liveness of one committed disclosure's original grant and run.
///
/// This is not a disclosure permit and contains no response bytes. The app
/// must retain the original authorized response and serialize revocation with
/// publication under its own admission fence. It must also check privacy,
/// attachment and request cancellation at the actual delivery boundary.
#[derive(Clone, Debug)]
pub struct DisclosureLiveness {
    pub(crate) connection: ConnectionId,
    pub(crate) deadline: Instant,
    pub(crate) grant: Arc<AtomicBool>,
    pub(crate) run: Arc<AtomicBool>,
    pub(crate) ancestors: Vec<Arc<AtomicBool>>,
}

impl DisclosureLiveness {
    pub fn is_live(&self, connection: ConnectionId, now: Instant) -> bool {
        self.connection == connection
            && now < self.deadline
            && self.grant.load(Ordering::Acquire)
            && self.run.load(Ordering::Acquire)
            && self
                .ancestors
                .iter()
                .all(|ancestor| ancestor.load(Ordering::Acquire))
    }
}

/// Per-channel idempotency reference; not a credential or execution authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct StartKey(pub [u8; 16]);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseRef {
    pub(crate) target: TargetRef,
    pub(crate) run: RunId,
    pub(crate) epoch: u64,
}

impl LeaseRef {
    pub fn target(self) -> TargetRef {
        self.target
    }

    pub fn run(self) -> RunId {
        self.run
    }

    pub fn ownership_epoch(self) -> u64 {
        self.epoch
    }
}

/// A prepared acquisition, not a lease; dropping it aborts without an effect.
pub struct LeaseTransition {
    pub(crate) connection: ConnectionId,
    pub(crate) run: RunId,
    pub(crate) target: TargetRef,
    pub(crate) observed_epoch: u64,
}

impl LeaseTransition {
    pub fn target(&self) -> TargetRef {
        self.target
    }

    pub fn observed_ownership_epoch(&self) -> u64 {
        self.observed_epoch
    }
}

/// Parameters are frozen at prepare time. Input bytes stay in the application;
/// the same application-computed digest must be checked at dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Discover {
        max_bytes: u32,
    },
    Observe {
        target: TargetRef,
        max_bytes: u32,
    },
    Control {
        lease: LeaseRef,
        input: Digest,
        bytes: u32,
        /// A frozen requirement, if the caller requires confirmed presentation.
        /// It cannot be removed between preparation and admission.
        presentation: Option<crate::PresentationRequirement>,
    },
    Result {
        max_bytes: u32,
    },
}

impl Operation {
    pub(crate) fn reserved_bytes(self) -> u32 {
        match self {
            Self::Discover { max_bytes }
            | Self::Observe { max_bytes, .. }
            | Self::Result { max_bytes } => max_bytes,
            Self::Control { .. } => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Error {
    Unauthorized,
    InvalidRequest,
    StaleTarget,
    Expired,
    Revoked,
    QuotaExceeded,
    Busy,
    Closed,
    BindingMismatch,
    Unavailable,
    ForegroundRefused,
    UnsettledHostCalls,
    UnsettledChildRuns,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Running,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
    Interrupted,
}

/// No strings or result content appear in status. Admission is NOT execution or
/// rollback; every admitted terminal write has unknown execution outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunStatus {
    pub state: RunState,
    pub failure: Option<Error>,
    pub admitted_effects: u32,
    pub execution_outcome_unknown: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Completion {
    Returned,
    Failed,
}

/// Fixed ceilings; lower per-run budgets are provided in RunSpec. Global usage
/// lasts for the broker lifetime, and native reapproval does not reset it.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub connections: usize,
    pub targets: usize,
    pub controller_registrations: usize,
    pub controller_registrations_per_connection: usize,
    pub retained_runs: usize,
    pub global_operations: u32,
    pub global_disclosed_bytes: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connections: 16,
            targets: 128,
            controller_registrations: 128,
            controller_registrations_per_connection: 16,
            retained_runs: 128,
            global_operations: 4096,
            global_disclosed_bytes: 16 * 1_048_576,
        }
    }
}
