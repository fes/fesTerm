use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

pub const MAX_GRANT_LIFETIME: Duration = Duration::from_secs(300);
pub const MAX_PAYLOAD_BYTES: u32 = 65_536;
pub const MAX_OBSERVATION_BYTES: u32 = 32_768;
pub const MAX_INPUT_BYTES: u32 = 16_384;
pub const MAX_SCOPE_TARGETS: usize = 32;

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

/// SHA-256 digest computed by the application over a frozen canonical artifact
/// or schema-validated canonical parameters, not an asserted worker hash.
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
    Direct { operation: DirectKind },
    Workflow { bundle: Digest, manifest: Digest },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationBinding {
    pub api_version: u32,
    pub invocation: Invocation,
    pub parameters: Digest,
}

/// Requested scopes are data, not authority. No wildcard or presentation sink
/// exists in this kernel. All results are returned only to the same connection.
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

/// Native UI decision. Deliberately neither serializable nor deserializable.
/// Only trusted application code may construct this after showing exact scopes,
/// workflow/parameter digests and the caller identity.
pub struct NativeApproval {
    pub(crate) request: GrantRequest,
    pub(crate) lifetime: Duration,
    pub(crate) handoffs: BTreeMap<TargetRef, BTreeSet<TargetRef>>,
}

impl NativeApproval {
    pub fn new(request: GrantRequest, lifetime: Duration) -> Self {
        Self {
            request,
            lifetime,
            handoffs: BTreeMap::new(),
        }
    }

    /// Native consent to unknown remote editing/TUI state AND disclosure of all
    /// listed sources through this exact account's execution/network authority.
    /// An empty local input queue is not evidence for granting this consent.
    pub fn accept_control_handoff(
        mut self,
        target: TargetRef,
        all_readable_sources: BTreeSet<TargetRef>,
    ) -> Self {
        self.handoffs.insert(target, all_readable_sources);
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
    pub retained_runs: usize,
    pub global_operations: u32,
    pub global_disclosed_bytes: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connections: 16,
            targets: 128,
            retained_runs: 128,
            global_operations: 4096,
            global_disclosed_bytes: 16 * 1_048_576,
        }
    }
}
