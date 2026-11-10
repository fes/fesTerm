use crate::{
    AttachmentId, ContractError, Epoch, GenerationId, HostId, LeaseId, LogicalSessionId,
    OperationId, Validate,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::io::{self, Write};

pub const HOST_MARKER: &str = "festerm-host-v1";
pub const HOST_VERSION: u16 = 1;
pub const CONTROL_FRAME_BYTES: usize = 64 * 1024;
pub const MAX_SESSIONS: usize = 64;
pub const PAGE_SIZE: usize = 16;
pub const MAX_RECIPE_BYTES: usize = 16 * 1024;
pub const CONTROL_DEADLINE_SECONDS: u16 = 15;
pub const AUTH_DEADLINE_SECONDS: u16 = 15;
pub const RECONNECT_GRACE_SECONDS: u16 = 60;

/// Captured from authenticated host capabilities for a NEW action. An uncertain
/// retry retains both fields; it must never substitute a newer epoch.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssuedOperation {
    pub id: OperationId,
    pub epoch: Epoch,
}

impl IssuedOperation {
    pub fn new(epoch: Epoch) -> Self {
        Self {
            id: OperationId::new(),
            epoch,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope<T> {
    pub protocol: String,
    pub version: u16,
    pub host: HostId,
    pub operation: OperationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuance_epoch: Option<Epoch>,
    pub body: T,
}

impl<T: Validate> Envelope<T> {
    pub fn new(host: HostId, operation: OperationId, body: T) -> Self {
        Self {
            protocol: HOST_MARKER.to_owned(),
            version: HOST_VERSION,
            host,
            operation,
            issuance_epoch: None,
            body,
        }
    }

    pub fn issued(host: HostId, operation: &IssuedOperation, body: T) -> Self {
        Self {
            issuance_epoch: Some(operation.epoch),
            ..Self::new(host, operation.id.clone(), body)
        }
    }
}

impl<T: Validate> Validate for Envelope<T> {
    fn validate(&self) -> Result<(), ContractError> {
        if self.protocol != HOST_MARKER || self.version != HOST_VERSION {
            return Err(ContractError::UnsupportedVersion);
        }
        if self.body.issuance_epoch_required() && self.issuance_epoch.is_none()
            || !self.body.issuance_epoch_allowed() && self.issuance_epoch.is_some()
        {
            return Err(ContractError::Malformed);
        }
        self.body.validate()
    }

    fn validate_wire_header(bytes: &[u8]) -> Result<(), ContractError> {
        #[derive(Deserialize)]
        struct Header {
            protocol: Option<String>,
            version: Option<u64>,
        }
        // Unknown bodies are skipped without constructing typed values. The
        // second pass still enforces the complete schema and unknown fields.
        let header: Header = serde_json::from_slice(bytes).map_err(|_| ContractError::Malformed)?;
        if header.protocol.is_none() && header.version.is_none() {
            return Err(ContractError::Malformed);
        }
        if header.protocol.as_deref() != Some(HOST_MARKER)
            || header.version != Some(u64::from(HOST_VERSION))
        {
            return Err(ContractError::UnsupportedVersion);
        }
        Ok(())
    }
}

/// Payload is one complete JSON object, carried by a length-prefixed channel.
/// The transport must reject a length over CONTROL_FRAME_BYTES *before* reading.
pub fn decode<T: DeserializeOwned + Validate>(bytes: &[u8]) -> Result<T, ContractError> {
    if bytes.is_empty() || bytes.len() > CONTROL_FRAME_BYTES {
        return Err(ContractError::Limit);
    }
    T::validate_wire_header(bytes)?;
    let value: T = serde_json::from_slice(bytes).map_err(|_| ContractError::Malformed)?;
    value.validate()?;
    Ok(value)
}

/// Never allocates an unbounded encoded frame, even for trusted caller mistakes.
pub fn encode<T: Serialize + Validate>(value: &T) -> Result<Vec<u8>, ContractError> {
    value.validate()?;
    struct Bounded(Vec<u8>);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.0.len().saturating_add(bytes.len()) > CONTROL_FRAME_BYTES {
                return Err(io::Error::other("host frame bound"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut buffer = Bounded(Vec::new());
    serde_json::to_writer(&mut buffer, value).map_err(|_| ContractError::Limit)?;
    Ok(buffer.0)
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub shell: Option<String>,
    pub args: Vec<String>,
    pub cwd: Option<String>,
}

impl Recipe {
    pub fn validate(&self) -> Result<(), ContractError> {
        <Self as Validate>::validate(self)
    }
}

fn text(value: &str, max: usize, empty: bool) -> bool {
    (empty || !value.is_empty()) && value.len() <= max && !value.chars().any(char::is_control)
}

impl Validate for Recipe {
    fn validate(&self) -> Result<(), ContractError> {
        if self.shell.as_ref().is_some_and(|s| !text(s, 4096, false))
            || self.cwd.as_ref().is_some_and(|s| !text(s, 4096, false))
            || self.args.len() > 32
            || self.args.iter().any(|arg| !text(arg, 1024, true))
            || (self.shell.is_none() && !self.args.is_empty())
        {
            return Err(ContractError::Malformed);
        }
        let bytes = self.shell.as_ref().map_or(0, String::len)
            + self.cwd.as_ref().map_or(0, String::len)
            + self.args.iter().map(String::len).sum::<usize>();
        if bytes > MAX_RECIPE_BYTES {
            return Err(ContractError::Limit);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Lifetime {
    Persistent {},
    Temporary { reconnect_grace_seconds: u16 },
}

impl Lifetime {
    pub fn validate(&self) -> Result<(), ContractError> {
        <Self as Validate>::validate(self)
    }
}

impl Validate for Lifetime {
    fn validate(&self) -> Result<(), ContractError> {
        match self {
            Self::Persistent {} => Ok(()),
            Self::Temporary {
                reconnect_grace_seconds,
            } if *reconnect_grace_seconds == RECONNECT_GRACE_SECONDS => Ok(()),
            _ => Err(ContractError::Malformed),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub logical: LogicalSessionId,
    pub generation: GenerationId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Creation {
    pub logical: LogicalSessionId,
    /// Future attaching tab; a temporary lease must be reserved for this
    /// attachment before spawning, even if the create reply is lost.
    pub attachment: AttachmentId,
    pub name: Option<String>,
    pub recipe: Recipe,
    pub lifetime: Lifetime,
}

impl Validate for Creation {
    fn validate(&self) -> Result<(), ContractError> {
        if self
            .name
            .as_ref()
            .is_some_and(|name| !text(name, 128, false))
        {
            return Err(ContractError::Malformed);
        }
        self.recipe.validate()?;
        self.lifetime.validate()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    pub snapshot: OperationId,
    pub offset: u16,
}

impl Validate for Cursor {
    fn validate(&self) -> Result<(), ContractError> {
        if self.offset == 0
            || usize::from(self.offset) >= MAX_SESSIONS
            || usize::from(self.offset) % PAGE_SIZE != 0
        {
            return Err(ContractError::Malformed);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetachKind {
    Intentional,
    TransportLost,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Capabilities {},
    List {
        cursor: Option<Cursor>,
    },
    Lookup {
        logical: LogicalSessionId,
    },
    /// Host must determine absence under the journal/admission boundary.
    Ensure {
        creation: Creation,
    },
    /// Refuses any existing or uncertain reservation; not an ensure alias.
    Create {
        creation: Creation,
    },
    Attach {
        target: Target,
        attachment: AttachmentId,
        takeover: bool,
    },
    Detach {
        target: Target,
        attachment: AttachmentId,
        lease: Option<LeaseId>,
        lease_epoch: Option<Epoch>,
        reason: DetachKind,
    },
    ExplicitEnd {
        target: Target,
    },
}

impl Validate for Request {
    fn issuance_epoch_required(&self) -> bool {
        matches!(self, Self::Ensure { .. } | Self::Create { .. })
    }

    fn issuance_epoch_allowed(&self) -> bool {
        self.issuance_epoch_required()
    }

    fn validate(&self) -> Result<(), ContractError> {
        match self {
            Self::List {
                cursor: Some(cursor),
            } => cursor.validate(),
            Self::Ensure { creation } | Self::Create { creation } => creation.validate(),
            Self::Detach {
                lease, lease_epoch, ..
            } if lease.is_some() != lease_epoch.is_some() => Err(ContractError::Malformed),
            _ => Ok(()),
        }
    }
}

/// Only current implemented/admitted operations may be advertised. Initial
/// defaults deliberately expose nothing, including inventory.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    /// Current protected host request-issuance namespace, not a consent grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuance_epoch: Option<Epoch>,
    pub inventory: bool,
    pub ensure: bool,
    pub create: bool,
    pub attach: bool,
    pub detach: bool,
    pub explicit_end: bool,
    pub temporary: bool,
    /// Independent protected execution grant. Ensure may still resume a live
    /// authorized session while this is false.
    pub creation_allowed: bool,
}

impl Capabilities {
    /// Negotiated operation support, independent of protected authorization.
    pub fn supports(&self, request: &Request) -> bool {
        match request {
            Request::Capabilities {} => true,
            Request::List { .. } | Request::Lookup { .. } => self.inventory,
            Request::Ensure { creation } => {
                self.ensure
                    && (!matches!(creation.lifetime, Lifetime::Temporary { .. }) || self.temporary)
            }
            Request::Create { creation } => {
                self.create
                    && (!matches!(creation.lifetime, Lifetime::Temporary { .. }) || self.temporary)
            }
            Request::Attach { .. } => self.attach,
            Request::Detach { .. } => self.detach,
            Request::ExplicitEnd { .. } => self.explicit_end,
        }
    }
}

impl Validate for Capabilities {
    fn validate(&self) -> Result<(), ContractError> {
        if (self.ensure || self.create || self.temporary) && !self.attach
            || self.temporary && !self.detach
            || self.creation_allowed && !(self.ensure || self.create)
            || (self.ensure || self.create) && self.issuance_epoch.is_none()
        {
            return Err(ContractError::Malformed);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSummary {
    pub target: Target,
    pub name: String,
    pub lifetime: Lifetime,
    pub attached: bool,
}

impl Validate for SessionSummary {
    fn validate(&self) -> Result<(), ContractError> {
        if !text(&self.name, 128, false) {
            return Err(ContractError::Malformed);
        }
        self.lifetime.validate()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LookupResult {
    Running {
        session: SessionSummary,
    },
    /// Only authoritative host metadata under current owner/grant admission
    /// can produce this result. A client's parsed copy is not a creation grant.
    Absent {
        logical: LogicalSessionId,
        authorization_epoch: Epoch,
    },
    Denied {},
    Offline {},
    Unknown {},
    Busy {},
    Incompatible {},
}

impl Validate for LookupResult {
    fn validate(&self) -> Result<(), ContractError> {
        match self {
            Self::Running { session } => session.validate(),
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnsureDisposition {
    Resumed,
    Created,
    Recreated,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporaryLease {
    pub lease: LeaseId,
    pub epoch: Epoch,
    pub attachment: AttachmentId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    Denied,
    Offline,
    Unknown,
    Busy,
    Incompatible,
    Stale,
    RecipeConflict,
    Revoked,
    OutcomeUncertain,
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Capabilities {
        capabilities: Capabilities,
    },
    Inventory {
        page: crate::InventoryPage,
    },
    Lookup {
        result: LookupResult,
    },
    Ensured {
        session: SessionSummary,
        disposition: EnsureDisposition,
        lease: Option<TemporaryLease>,
    },
    Attached {
        target: Target,
        attachment: AttachmentId,
        lease: Option<TemporaryLease>,
    },
    Detached {
        target: Target,
    },
    Ended {
        target: Target,
    },
    Refused {
        reason: Refusal,
    },
}

impl Validate for Response {
    fn validate(&self) -> Result<(), ContractError> {
        match self {
            Self::Capabilities { capabilities } => capabilities.validate(),
            Self::Inventory { page } => page.validate(),
            Self::Lookup { result } => result.validate(),
            Self::Ensured { session, lease, .. } => {
                session.validate()?;
                if matches!(session.lifetime, Lifetime::Temporary { .. }) != lease.is_some() {
                    return Err(ContractError::Malformed);
                }
                Ok(())
            }
            Self::Attached {
                attachment,
                lease: Some(lease),
                ..
            } if *attachment != lease.attachment => Err(ContractError::Malformed),
            _ => Ok(()),
        }
    }
}

impl Envelope<Request> {
    pub fn issued_operation(&self) -> Result<IssuedOperation, ContractError> {
        self.validate()?;
        if !self.body.issuance_epoch_required() {
            return Err(ContractError::Malformed);
        }
        Ok(IssuedOperation {
            id: self.operation.clone(),
            epoch: self.issuance_epoch.ok_or(ContractError::Malformed)?,
        })
    }
}

impl Envelope<Response> {
    /// Correlates a decoded response to the captured request before adopting
    /// a target/lease. This is not host authentication or authorization.
    pub fn validate_for(&self, request: &Envelope<Request>) -> Result<(), ContractError> {
        self.validate()?;
        request.validate()?;
        if self.host != request.host
            || self.operation != request.operation
            || self.issuance_epoch != request.issuance_epoch
        {
            return Err(ContractError::Stale);
        }

        let matches = match (&request.body, &self.body) {
            (_, Response::Refused { .. }) => true,
            (Request::Capabilities {}, Response::Capabilities { .. }) => true,
            (Request::List { cursor }, Response::Inventory { page }) => {
                let offset = cursor
                    .as_ref()
                    .map_or(0, |cursor| usize::from(cursor.offset));
                cursor
                    .as_ref()
                    .is_none_or(|cursor| cursor.snapshot == page.snapshot)
                    && page
                        .next
                        .as_ref()
                        .is_none_or(|next| usize::from(next.offset) == offset + PAGE_SIZE)
            }
            (Request::Lookup { logical }, Response::Lookup { result }) => match result {
                LookupResult::Running { session } => session.target.logical == *logical,
                LookupResult::Absent {
                    logical: actual, ..
                } => actual == logical,
                _ => true,
            },
            (
                Request::Ensure { creation } | Request::Create { creation },
                Response::Ensured {
                    session,
                    disposition,
                    lease,
                },
            ) => {
                session.target.logical == creation.logical
                    && lease
                        .as_ref()
                        .is_none_or(|lease| lease.attachment == creation.attachment)
                    && (*disposition == EnsureDisposition::Resumed
                        || session.lifetime == creation.lifetime)
                    && (!matches!(request.body, Request::Create { .. })
                        || *disposition != EnsureDisposition::Resumed)
            }
            (
                Request::Attach {
                    target, attachment, ..
                },
                Response::Attached {
                    target: actual,
                    attachment: actual_attachment,
                    ..
                },
            ) => target == actual && attachment == actual_attachment,
            (Request::Detach { target, .. }, Response::Detached { target: actual })
            | (Request::ExplicitEnd { target }, Response::Ended { target: actual }) => {
                target == actual
            }
            _ => false,
        };
        if !matches {
            return Err(ContractError::Stale);
        }
        Ok(())
    }
}
