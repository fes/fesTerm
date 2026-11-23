//! Sequential rooted-file client for an already verified, separate legacy or
//! HOST file stream (ADR 0067). This does not authenticate HOST roles or attach a
//! terminal. Callers own local staging/publication and explicit replacement consent.
use crate::{
    file_protocol::{self as rpc, Operation, Result as Reply},
    protocol::{Target, DESCRIPTION, DISCOVER},
    wire::{write_frame, DEADLINE},
};
use std::{collections::HashMap, fmt, io, sync::Arc};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub enum Error {
    Closed,
    Cancelled,
    Deadline,
    Invalid,
    Protocol,
    Transport(io::ErrorKind),
    LocalIo(io::ErrorKind),
    /// Local I/O ended, but closing its connection-local transfer handle was
    /// not confirmed. Preserve both failures; never retry the transfer.
    CleanupFailed {
        operation: Box<Error>,
        cleanup: Box<Error>,
    },
    Remote {
        code: rpc::ErrorCode,
        recovery_path: Option<String>,
    },
    /// The finish may have published. Never reconnect and replay it.
    UncertainFinish(Box<Error>),
    /// The reviewed deletion may have completed. Never replay it.
    UncertainDelete(Box<Error>),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => f.write_str("rooted file stream is closed"),
            Self::Cancelled => f.write_str("rooted file operation cancelled"),
            Self::Deadline => f.write_str("rooted file operation deadline exceeded"),
            Self::Invalid => f.write_str("invalid rooted file operation"),
            Self::Protocol => f.write_str("rooted file protocol refused"),
            Self::Transport(kind) => write!(f, "rooted file transport failed ({kind:?})"),
            Self::LocalIo(kind) => write!(f, "local transfer I/O failed ({kind:?})"),
            Self::CleanupFailed { operation, cleanup } => write!(
                f,
                "{operation}; transfer handle cleanup could not be confirmed: {cleanup}"
            ),
            Self::Remote { code, .. } => write!(f, "rooted file operation refused ({code:?})"),
            Self::UncertainDelete(cause) => {
                write!(f, "delete result is uncertain; do not replay: {cause}")
            }
            Self::UncertainFinish(cause) => write!(
                f,
                "upload publication is uncertain; do not replay finish: {cause}"
            ),
        }
    }
}
impl std::error::Error for Error {}
type Result<T> = std::result::Result<T, Error>;

/// Fixed negative probes for the opt-in provider/native-files lab only.
/// Normal shipping builds do not enable the existing `testing` feature.
#[cfg(feature = "testing")]
#[derive(Clone, Copy, Debug)]
pub enum BoundaryProbe {
    ParentTraversal,
    Backslash,
    DrivePath,
    ReservedName,
    ReadOnlyMkdir,
    ReadOnlyUpload,
}

#[cfg(feature = "testing")]
fn validate_boundary_leaf(leaf: &str) -> Result<()> {
    if !leaf.starts_with("festerm-boundary-")
        || !(32..=64).contains(&leaf.len())
        || !leaf
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(Error::Invalid);
    }
    Ok(())
}

#[cfg(feature = "testing")]
fn boundary_operation(probe: BoundaryProbe, leaf: &str) -> Result<Operation> {
    validate_boundary_leaf(leaf)?;
    Ok(match probe {
        BoundaryProbe::ParentTraversal => Operation::OpenRead {
            path: format!("/../{leaf}/outside.txt"),
        },
        BoundaryProbe::Backslash => Operation::OpenRead {
            path: format!("/{leaf}\\outside.txt"),
        },
        BoundaryProbe::DrivePath => Operation::OpenRead {
            path: format!("C:/{leaf}/outside.txt"),
        },
        BoundaryProbe::ReservedName => Operation::OpenRead {
            path: format!("/{leaf}/.festerm-transfer-probe"),
        },
        BoundaryProbe::ReadOnlyMkdir => Operation::Mkdir {
            path: format!("/{leaf}/denied-mkdir"),
        },
        BoundaryProbe::ReadOnlyUpload => Operation::BeginUpload {
            path: format!("/{leaf}/denied-upload"),
            size: "0".into(),
            expected: None,
        },
    })
}

/// A connection-local handle; it cannot authorize a coincidentally equal handle
/// on another stream. EOF does not close it: call `close` explicitly.
pub struct Handle {
    id: u32,
    connection: Arc<()>,
    issued: u32,
}
enum State {
    EntryReview,
    Directory,
    Read { path: String, size: u64 },
    Upload { size: u64, written: u64 },
}
pub struct Page {
    pub entries: Vec<rpc::Entry>,
    pub done: bool,
    pub skipped: u16,
}
pub struct Chunk {
    pub bytes: Vec<u8>,
    pub eof: bool,
}

pub struct Client<S> {
    stream: Option<S>,
    capabilities: rpc::Capabilities,
    operations: Option<rpc::OperationCapabilities>,
    next_id: u32,
    handles: HashMap<u32, (u32, State)>,
    connection: Arc<()>,
    cancel: CancellationToken,
    #[cfg(feature = "host-routing")]
    host_guard: Option<crate::host_client::ordinary::OrdinaryAccountGuard>,
}

async fn bounded<T>(
    cancel: &CancellationToken,
    work: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    bounded_until(cancel, tokio::time::Instant::now() + DEADLINE, work).await
}
async fn bounded_until<T>(
    cancel: &CancellationToken,
    deadline: tokio::time::Instant,
    work: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    tokio::select! { biased;
        _ = cancel.cancelled() => Err(Error::Cancelled),
        result = tokio::time::timeout_at(deadline, work) => result.map_err(|_| Error::Deadline)?,
    }
}
fn transport(error: io::Error) -> Error {
    Error::Transport(error.kind())
}
fn json<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|_| Error::Invalid)
}
// Check the expected kind and this operation's bound BEFORE allocating, rather
// than accepting another bridge frame's larger allocation allowance.
async fn frame(stream: &mut (impl AsyncRead + Unpin), kind: u8, maximum: usize) -> Result<Vec<u8>> {
    let mut header = [0; 5];
    stream.read_exact(&mut header).await.map_err(transport)?;
    let length = u32::from_be_bytes(header[1..].try_into().map_err(|_| Error::Protocol)?) as usize;
    if header[0] != kind || length > maximum {
        return Err(Error::Protocol);
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await.map_err(transport)?;
    Ok(bytes)
}
fn capabilities(value: &rpc::Capabilities) -> Result<()> {
    if value.version != rpc::VERSION
        || value.chunk_limit == 0
        || value.chunk_limit > rpc::CHUNK_LIMIT
        || value.page_limit == 0
        || value.page_limit > rpc::PAGE_LIMIT
        || value.handle_limit == 0
        || value.handle_limit > rpc::HANDLE_LIMIT
    {
        return Err(Error::Protocol);
    }
    Ok(())
}
fn target(value: &Target) -> Result<()> {
    if value.pid == 0
        || value.name.is_empty()
        || value.name.len() > 128
        || value.name.chars().any(char::is_control)
        || value
            .generation
            .parse::<u128>()
            .ok()
            .is_none_or(|n| n.to_string() != value.generation)
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn path(value: &str) -> Result<()> {
    if rpc::valid_path(value) {
        Ok(())
    } else {
        Err(Error::Invalid)
    }
}
fn metadata(value: &rpc::Metadata) -> Result<u64> {
    let size = rpc::decimal(&value.size).ok_or(Error::Protocol)?;
    if value
        .modified_ms
        .as_ref()
        .is_some_and(|value| rpc::decimal(value).is_none())
        || matches!(value.kind, rpc::Kind::Directory) && size != 0
    {
        return Err(Error::Protocol);
    }
    Ok(size)
}
fn recovery_path(value: &str) -> bool {
    let Some((parent, name)) = value.rsplit_once('/') else {
        return false;
    };
    name.len() <= 255
        && name.to_ascii_lowercase().starts_with(".festerm-transfer-")
        && rpc::valid_name(&name.replacen(".festerm-transfer-", "recovery-", 1))
        && rpc::valid_path(&format!("{parent}/recovery"))
}
async fn response(stream: &mut (impl AsyncRead + Unpin), id: u32) -> Result<rpc::Response> {
    let bytes = frame(stream, rpc::RESPONSE, rpc::JSON_LIMIT).await?;
    let value: rpc::Response = serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)?;
    if value.id != id
        || value.result.is_some() == value.error.is_some()
        || value.recovery_path.as_ref().is_some_and(|path| {
            value.error != Some(rpc::ErrorCode::RecoveryRequired) || !recovery_path(path)
        })
    {
        return Err(Error::Protocol);
    }
    Ok(value)
}

impl<S: AsyncRead + AsyncWrite + Unpin> Client<S> {
    /// Consumes one already host-key-verified discovery stream and checks its
    /// exact advertised target. `None` means disabled, never terminal authority.
    pub async fn discover(
        mut stream: S,
        expected: &Target,
        cancel: &CancellationToken,
    ) -> Result<Option<rpc::Capabilities>> {
        target(expected)?;
        bounded(cancel, async {
            write_frame(&mut stream, DISCOVER, &rpc::DISCOVERY_VERSION.to_be_bytes())
                .await
                .map_err(transport)?;
            let bytes = frame(&mut stream, DESCRIPTION, 4096).await?;
            let value: rpc::Discovery =
                serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)?;
            if value.version != rpc::DISCOVERY_VERSION || value.target != *expected {
                return Err(Error::Protocol);
            }
            if let Some(value) = &value.filesystem {
                capabilities(value)?;
            }
            Ok(value.filesystem)
        })
        .await
    }
    /// Consumes a new separate, already host-key-verified stream. No reconnect,
    /// credential lookup or terminal admission occurs in this client.
    pub async fn open(mut stream: S, expected: Target, cancel: CancellationToken) -> Result<Self> {
        target(&expected)?;
        let capabilities = bounded(&cancel, async {
            write_frame(
                &mut stream,
                rpc::OPEN,
                &json(&rpc::Open {
                    version: rpc::VERSION,
                    target: expected,
                })?,
            )
            .await
            .map_err(transport)?;
            let value = response(&mut stream, 0).await?;
            match (value.result, value.error) {
                (
                    Some(Reply::Ready {
                        capabilities: value,
                    }),
                    None,
                ) => {
                    capabilities(&value)?;
                    Ok(value)
                }
                (None, Some(code)) => Err(Error::Remote {
                    code,
                    recovery_path: value.recovery_path,
                }),
                _ => Err(Error::Protocol),
            }
        })
        .await?;
        Ok(Self {
            stream: Some(stream),
            capabilities,
            operations: None,
            next_id: 0,
            handles: HashMap::new(),
            connection: Arc::new(()),
            cancel,
            #[cfg(feature = "host-routing")]
            host_guard: None,
        })
    }
    /// Only after ordinary Files-role authentication on the actual verified
    /// channel. No discovery, proof, credentials or terminal fallback occurs.
    /// None confirms the authenticated scope has files disabled.
    pub async fn open_host(
        stream: S,
        host: &festerm_host_protocol::HostId,
        installation: &festerm_host_protocol::HostId,
        cancel: CancellationToken,
    ) -> Result<Option<(Self, rpc::HostScope)>> {
        Self::open_host_checked(
            stream,
            host,
            installation,
            None,
            || Ok(()),
            cancel,
            rpc::HOST_VERSION,
        )
        .await
    }

    /// Explicit reviewed-operation negotiation; never falls back to Open2.
    pub async fn open_host_reviewed(
        stream: S,
        host: &festerm_host_protocol::HostId,
        installation: &festerm_host_protocol::HostId,
        cancel: CancellationToken,
    ) -> Result<Option<(Self, rpc::HostScope)>> {
        Self::open_host_checked(
            stream,
            host,
            installation,
            None,
            || Ok(()),
            cancel,
            rpc::HOST_REVIEWED_VERSION,
        )
        .await
    }

    #[cfg(feature = "host-routing")]
    pub(crate) async fn open_host_guarded(
        stream: S,
        host: &festerm_host_protocol::HostId,
        installation: &festerm_host_protocol::HostId,
        pinned: Option<&rpc::HostScope>,
        guard: crate::host_client::ordinary::OrdinaryAccountGuard,
        cancel: CancellationToken,
    ) -> Result<Option<(Self, rpc::HostScope)>> {
        Self::open_host_guarded_version(
            stream,
            host,
            installation,
            pinned,
            guard,
            cancel,
            rpc::HOST_VERSION,
        )
        .await
    }
    #[cfg(feature = "host-routing")]
    pub(crate) async fn open_host_guarded_reviewed(
        stream: S,
        host: &festerm_host_protocol::HostId,
        installation: &festerm_host_protocol::HostId,
        pinned: Option<&rpc::HostScope>,
        guard: crate::host_client::ordinary::OrdinaryAccountGuard,
        cancel: CancellationToken,
    ) -> Result<Option<(Self, rpc::HostScope)>> {
        Self::open_host_guarded_version(
            stream,
            host,
            installation,
            pinned,
            guard,
            cancel,
            rpc::HOST_REVIEWED_VERSION,
        )
        .await
    }

    #[cfg(feature = "host-routing")]
    async fn open_host_guarded_version(
        stream: S,
        host: &festerm_host_protocol::HostId,
        installation: &festerm_host_protocol::HostId,
        pinned: Option<&rpc::HostScope>,
        guard: crate::host_client::ordinary::OrdinaryAccountGuard,
        cancel: CancellationToken,
        version: u16,
    ) -> Result<Option<(Self, rpc::HostScope)>> {
        let current = guard.clone();
        let opened = Self::open_host_checked(
            stream,
            host,
            installation,
            pinned,
            move || current.check().map_err(|_| Error::Cancelled),
            cancel,
            version,
        )
        .await?;
        Ok(opened.map(|(mut client, scope)| {
            client.host_guard = Some(guard);
            (client, scope)
        }))
    }

    async fn open_host_checked(
        mut stream: S,
        host: &festerm_host_protocol::HostId,
        installation: &festerm_host_protocol::HostId,
        pinned: Option<&rpc::HostScope>,
        current: impl Fn() -> Result<()>,
        cancel: CancellationToken,
        version: u16,
    ) -> Result<Option<(Self, rpc::HostScope)>> {
        if !matches!(version, rpc::HOST_VERSION | rpc::HOST_REVIEWED_VERSION) {
            return Err(Error::Invalid);
        }
        current()?;
        let description = bounded(&cancel, async {
            let bytes = frame(&mut stream, rpc::HOST_DESCRIPTION, 4096).await?;
            let description: rpc::HostDescription =
                serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)?;
            if description.version != rpc::HOST_VERSION
                || &description.scope.host != host
                || &description.scope.installation != installation
                || description.scope.validate().is_err()
                || pinned.is_some_and(|expected| expected != &description.scope)
            {
                return Err(Error::Protocol);
            }
            if let Some(caps) = &description.filesystem {
                capabilities(caps)?;
            }
            Ok(description)
        })
        .await?;
        current()?;
        let Some(caps) = description.filesystem else {
            return Ok(None);
        };
        let operations = bounded(&cancel, async {
            write_frame(
                &mut stream,
                rpc::OPEN,
                &json(&rpc::HostOpen {
                    version,
                    scope: description.scope.clone(),
                })?,
            )
            .await
            .map_err(transport)?;
            match response(&mut stream, 0).await? {
                rpc::Response {
                    result: Some(Reply::Ready { capabilities }),
                    error: None,
                    ..
                } if version == rpc::HOST_VERSION && capabilities == caps => Ok(None),
                rpc::Response {
                    result: Some(Reply::ReadyV3 { capabilities }),
                    error: None,
                    ..
                } if version == rpc::HOST_REVIEWED_VERSION && capabilities.validate(&caps) => {
                    Ok(Some(capabilities))
                }
                rpc::Response {
                    result: None,
                    error: Some(code),
                    recovery_path,
                    ..
                } => Err(Error::Remote {
                    code,
                    recovery_path,
                }),
                _ => Err(Error::Protocol),
            }
        })
        .await?;
        current()?;
        Ok(Some((
            Self {
                stream: Some(stream),
                capabilities: caps,
                operations,
                next_id: 0,
                handles: HashMap::new(),
                connection: Arc::new(()),
                cancel,
                #[cfg(feature = "host-routing")]
                host_guard: None,
            },
            description.scope,
        )))
    }
    pub fn capabilities(&self) -> &rpc::Capabilities {
        &self.capabilities
    }
    /// Observed operation capability only; never proof of current remote consent.
    pub fn delete_advertised(&self) -> bool {
        self.operations
            .as_ref()
            .is_some_and(|ops| ops.allows_delete())
    }

    pub fn available(&self) -> bool {
        self.stream.is_some() && !self.cancel.is_cancelled()
    }
    pub fn cancel(&mut self) {
        self.cancel.cancel();
        self.stream = None;
        self.handles.clear();
    }
    /// Negative-only local fence before caller-owned local publication. Checks
    /// stream/child cancellation and the captured account guard when present.
    /// An unguarded legacy client does not verify an account. This performs no
    /// network I/O and proves no remote liveness, current consent, or new grant.
    #[cfg(feature = "host-routing")]
    pub fn check_current(&mut self) -> Result<()> {
        self.ready()
    }
    fn ready(&mut self) -> Result<()> {
        #[cfg(feature = "host-routing")]
        if self
            .host_guard
            .as_ref()
            .is_some_and(|guard| guard.check().is_err())
        {
            self.cancel();
            return Err(Error::Cancelled);
        }
        if self.cancel.is_cancelled() {
            self.stream = None;
            return Err(Error::Cancelled);
        }
        if self.stream.is_none() {
            return Err(Error::Closed);
        }
        Ok(())
    }
    fn protocol<T>(&mut self) -> Result<T> {
        self.stream = None;
        Err(Error::Protocol)
    }
    fn state(&self, handle: &Handle) -> Result<&State> {
        if !Arc::ptr_eq(&self.connection, &handle.connection) {
            return Err(Error::Invalid);
        }
        let (issued, state) = self.handles.get(&handle.id).ok_or(Error::Invalid)?;
        if *issued != handle.issued {
            return Err(Error::Invalid);
        }
        Ok(state)
    }
    fn capacity(&self) -> Result<()> {
        if self.handles.len() >= self.capabilities.handle_limit {
            Err(Error::Invalid)
        } else {
            Ok(())
        }
    }
    fn insert(&mut self, id: u32, state: State) -> Result<Handle> {
        if id == 0 || self.handles.contains_key(&id) {
            return self.protocol();
        }
        self.handles.insert(id, (self.next_id, state));
        Ok(Handle {
            id,
            connection: self.connection.clone(),
            issued: self.next_id,
        })
    }
    async fn exchange(
        &mut self,
        operation: Operation,
        data: Option<&[u8]>,
    ) -> Result<(Reply, Option<Vec<u8>>)> {
        self.ready()?;
        let Some(id) = self.next_id.checked_add(1) else {
            return self.protocol();
        };
        self.next_id = id;
        let bytes = json(&rpc::Request { id, operation })?;
        // Ownership moves into this future before I/O. Dropping the future,
        // cancellation, timeout or malformed input closes the stream immediately.
        let mut stream = self.stream.take().ok_or(Error::Closed)?;
        let received = bounded(&self.cancel, async {
            write_frame(&mut stream, rpc::REQUEST, &bytes)
                .await
                .map_err(transport)?;
            if let Some(data) = data {
                let mut bytes = Vec::with_capacity(data.len() + 4);
                bytes.extend_from_slice(&id.to_be_bytes());
                bytes.extend_from_slice(data);
                write_frame(&mut stream, rpc::DATA, &bytes)
                    .await
                    .map_err(transport)?;
            }
            let value = response(&mut stream, id).await?;
            let data = if let Some(Reply::Read { length, .. }) = &value.result {
                if *length as usize > self.capabilities.chunk_limit {
                    return Err(Error::Protocol);
                }
                let bytes = frame(&mut stream, rpc::DATA, *length as usize + 4).await?;
                if bytes.len() != *length as usize + 4 || bytes[..4] != id.to_be_bytes() {
                    return Err(Error::Protocol);
                }
                Some(bytes[4..].to_vec())
            } else {
                None
            };
            Ok((value, data))
        })
        .await?;
        self.stream = Some(stream);
        self.ready()?; // account/current-scope fence before disclosing the reply
        match received {
            (
                rpc::Response {
                    result: Some(reply),
                    error: None,
                    ..
                },
                data,
            ) => Ok((reply, data)),
            (
                rpc::Response {
                    result: None,
                    error: Some(code),
                    recovery_path,
                    ..
                },
                _,
            ) => Err(Error::Remote {
                code,
                recovery_path,
            }),
            _ => self.protocol(),
        }
    }
    /// Sends only a fixed lab negative probe, bypassing local path/write UX
    /// checks so a *remote* refusal can be distinguished from local rejection.
    /// Keeps exchange's account, cancellation, framing and deadline fences.
    #[cfg(feature = "testing")]
    pub async fn probe_boundary_refusal(
        &mut self,
        probe: BoundaryProbe,
        owned_leaf: &str,
    ) -> Result<rpc::ErrorCode> {
        let operation = boundary_operation(probe, owned_leaf)?;
        if matches!(
            probe,
            BoundaryProbe::ReadOnlyMkdir | BoundaryProbe::ReadOnlyUpload
        ) && self.capabilities.write
        {
            return Err(Error::Invalid);
        }
        match self.exchange(operation, None).await {
            Err(Error::Remote { code, .. }) => Ok(code),
            Err(error) => Err(error),
            // Unexpected acceptance is a failed test, never Finish/retry. Drop
            // the channel and let the server's existing handle cleanup run.
            Ok(_) => self.protocol(),
        }
    }

    /// Fixed lab-only deletion-preparation denial on the owned public canary.
    /// Never sends Delete/confirmation or exposes an arbitrary path or request.
    #[cfg(feature = "testing")]
    pub async fn probe_delete_off_refusal(&mut self, owned_leaf: &str) -> Result<rpc::ErrorCode> {
        validate_boundary_leaf(owned_leaf)?;
        self.ready()?;
        if !self.capabilities.write || self.delete_advertised() {
            return Err(Error::Invalid);
        }
        let path = format!("/{owned_leaf}/canary.txt");
        let info = self.stat(&path).await?;
        if !matches!(info.kind, rpc::Kind::File) {
            return self.protocol();
        }
        match self
            .exchange(
                Operation::ReviewEntry {
                    path,
                    purpose: rpc::ReviewPurpose::Delete,
                },
                None,
            )
            .await
        {
            Err(Error::Remote { code, .. }) => Ok(code),
            Err(error) => Err(error),
            // An accepted review is a failed test, never Delete/Close/retry.
            Ok(_) => self.protocol(),
        }
    }

    pub async fn ping(&mut self) -> Result<()> {
        match self.exchange(Operation::Ping {}, None).await? {
            (Reply::Pong, None) => Ok(()),
            _ => self.protocol(),
        }
    }
    pub async fn stat(&mut self, value: &str) -> Result<rpc::Metadata> {
        path(value)?;
        match self
            .exchange(Operation::Stat { path: value.into() }, None)
            .await?
        {
            (Reply::Stat { metadata: value }, None) if metadata(&value).is_ok() => Ok(value),
            _ => self.protocol(),
        }
    }
    pub async fn open_directory(&mut self, value: &str) -> Result<Handle> {
        path(value)?;
        self.capacity()?;
        match self
            .exchange(Operation::OpenDirectory { path: value.into() }, None)
            .await?
        {
            (Reply::Directory { handle }, None) => self.insert(handle, State::Directory),
            _ => self.protocol(),
        }
    }
    pub async fn read_directory(&mut self, handle: &Handle, limit: u16) -> Result<Page> {
        if !matches!(self.state(handle)?, State::Directory)
            || limit == 0
            || limit > self.capabilities.page_limit
        {
            return Err(Error::Invalid);
        }
        match self
            .exchange(
                Operation::ReadDirectory {
                    handle: handle.id,
                    limit,
                },
                None,
            )
            .await?
        {
            (
                Reply::Entries {
                    entries,
                    done,
                    skipped,
                },
                None,
            ) if entries.len() <= usize::from(limit)
                && entries.len() + usize::from(skipped) <= usize::from(rpc::PAGE_LIMIT) * 4
                && entries
                    .iter()
                    .all(|entry| rpc::valid_name(&entry.name) && !entry.name.contains('/')) =>
            {
                Ok(Page {
                    entries,
                    done,
                    skipped,
                })
            }
            _ => self.protocol(),
        }
    }
    pub async fn review_entry(&mut self, value: &str) -> Result<(Handle, rpc::Metadata)> {
        self.ready()?;
        path(value)?;
        if !self.delete_advertised() {
            return Err(Error::Invalid);
        }
        self.capacity()?;
        match self
            .exchange(
                Operation::ReviewEntry {
                    path: value.into(),
                    purpose: rpc::ReviewPurpose::Delete,
                },
                None,
            )
            .await?
        {
            (
                Reply::EntryReview {
                    handle,
                    metadata: info,
                },
                None,
            ) if matches!(info.kind, rpc::Kind::File | rpc::Kind::Directory)
                && metadata(&info).is_ok() =>
            {
                let handle = self.insert(handle, State::EntryReview)?;
                Ok((handle, info))
            }
            _ => self.protocol(),
        }
    }
    /// Consumes one exact reviewed entry. Missing results are never replayed.
    pub async fn delete_reviewed(&mut self, review: &Handle) -> Result<()> {
        self.ready()?;
        if !self.delete_advertised() || !matches!(self.state(review)?, State::EntryReview) {
            return Err(Error::Invalid);
        }
        self.handles.remove(&review.id);
        match self
            .exchange(Operation::Delete { review: review.id }, None)
            .await
        {
            Ok((Reply::Deleted, None)) => Ok(()),
            Err(
                error @ Error::Remote {
                    code: rpc::ErrorCode::RecoveryRequired,
                    ..
                },
            ) => {
                self.cancel();
                Err(Error::UncertainDelete(Box::new(error)))
            }
            Err(error @ Error::Remote { .. }) => Err(error),
            Err(error) => {
                self.cancel();
                Err(Error::UncertainDelete(Box::new(error)))
            }
            _ => {
                self.cancel();
                Err(Error::UncertainDelete(Box::new(Error::Protocol)))
            }
        }
    }

    pub async fn open_read(&mut self, value: &str) -> Result<(Handle, rpc::Metadata)> {
        path(value)?;
        self.capacity()?;
        match self
            .exchange(Operation::OpenRead { path: value.into() }, None)
            .await?
        {
            (
                Reply::File {
                    handle,
                    metadata: info,
                },
                None,
            ) if matches!(info.kind, rpc::Kind::File) && metadata(&info).is_ok() => {
                let handle = self.insert(
                    handle,
                    State::Read {
                        path: value.into(),
                        size: metadata(&info)?,
                    },
                )?;
                Ok((handle, info))
            }
            _ => self.protocol(),
        }
    }
    pub async fn read(&mut self, handle: &Handle, offset: u64, length: u32) -> Result<Chunk> {
        let size = match self.state(handle)? {
            State::Read { size, .. } => *size,
            _ => return Err(Error::Invalid),
        };
        if length == 0 || length as usize > self.capabilities.chunk_limit || offset > size {
            return Err(Error::Invalid);
        }
        let expected = (size - offset).min(u64::from(length));
        match self
            .exchange(
                Operation::Read {
                    handle: handle.id,
                    offset: offset.to_string(),
                    length,
                },
                None,
            )
            .await?
        {
            (
                Reply::Read {
                    offset: got,
                    length: got_length,
                    eof,
                },
                Some(bytes),
            ) if rpc::decimal(&got) == Some(offset)
                && u64::from(got_length) == expected
                && bytes.len() as u64 == expected
                && eof == (offset + expected == size) =>
            {
                Ok(Chunk { bytes, eof })
            }
            _ => self.protocol(),
        }
    }
    pub async fn begin_upload(
        &mut self,
        value: &str,
        size: u64,
        expected: Option<&Handle>,
    ) -> Result<Handle> {
        path(value)?;
        self.capacity()?;
        if !self.capabilities.write
            || self
                .handles
                .values()
                .any(|(_, state)| matches!(state, State::Upload { .. }))
        {
            return Err(Error::Invalid);
        }
        if let Some(handle) = expected {
            if !matches!(self.state(handle)?, State::Read { path, .. } if path == value) {
                return Err(Error::Invalid);
            }
        }
        match self
            .exchange(
                Operation::BeginUpload {
                    path: value.into(),
                    size: size.to_string(),
                    expected: expected.map(|handle| handle.id),
                },
                None,
            )
            .await?
        {
            (Reply::Upload { handle }, None) => {
                self.insert(handle, State::Upload { size, written: 0 })
            }
            _ => self.protocol(),
        }
    }
    pub async fn write(&mut self, handle: &Handle, bytes: &[u8]) -> Result<u64> {
        let (size, written) = match self.state(handle)? {
            State::Upload { size, written } => (*size, *written),
            _ => return Err(Error::Invalid),
        };
        let next = written
            .checked_add(bytes.len() as u64)
            .ok_or(Error::Invalid)?;
        if bytes.is_empty() || bytes.len() > self.capabilities.chunk_limit || next > size {
            return Err(Error::Invalid);
        }
        match self
            .exchange(
                Operation::Write {
                    handle: handle.id,
                    offset: written.to_string(),
                    length: bytes.len() as u32,
                },
                Some(bytes),
            )
            .await?
        {
            (Reply::Written { offset }, None) if rpc::decimal(&offset) == Some(next) => {
                self.handles.insert(
                    handle.id,
                    (
                        handle.issued,
                        State::Upload {
                            size,
                            written: next,
                        },
                    ),
                );
                Ok(next)
            }
            _ => self.protocol(),
        }
    }
    pub async fn finish_upload(&mut self, handle: &Handle) -> Result<u64> {
        self.ready()?;
        let size = match self.state(handle)? {
            State::Upload { size, written } if size == written => *size,
            _ => return Err(Error::Invalid),
        };
        self.handles.remove(&handle.id);
        match self
            .exchange(Operation::FinishUpload { handle: handle.id }, None)
            .await
        {
            Ok((Reply::Finished { size: got }, None)) if rpc::decimal(&got) == Some(size) => {
                Ok(size)
            }
            Err(error @ Error::Remote { .. }) => Err(error),
            Err(error) => Err(Error::UncertainFinish(Box::new(error))),
            _ => {
                self.stream = None;
                Err(Error::UncertainFinish(Box::new(Error::Protocol)))
            }
        }
    }
    pub async fn close(&mut self, handle: &Handle) -> Result<()> {
        self.state(handle)?;
        match self
            .exchange(Operation::Close { handle: handle.id }, None)
            .await?
        {
            (Reply::Closed, None) => {
                self.handles.remove(&handle.id);
                Ok(())
            }
            _ => self.protocol(),
        }
    }
    pub async fn mkdir(&mut self, value: &str) -> Result<()> {
        path(value)?;
        if !self.capabilities.write {
            return Err(Error::Invalid);
        }
        match self
            .exchange(Operation::Mkdir { path: value.into() }, None)
            .await?
        {
            (Reply::Created, None) => Ok(()),
            _ => self.protocol(),
        }
    }
    /// Streams to a caller-owned sink. The caller owns safe local staging and
    /// publication; this API never removes or publishes a local pathname.
    pub async fn download(
        &mut self,
        value: &str,
        sink: &mut (impl AsyncWrite + Unpin),
    ) -> Result<u64> {
        let transfer = Transfer {
            client: self,
            complete: false,
        };
        let (handle, info) = transfer.client.open_read(value).await?;
        let size = metadata(&info)?;
        let mut offset = 0;
        loop {
            let chunk = transfer
                .client
                .read(
                    &handle,
                    offset,
                    transfer.client.capabilities.chunk_limit as u32,
                )
                .await?;
            transfer.client.ready()?;
            let deadline = tokio::time::Instant::now() + DEADLINE;
            let written = bounded_until(&transfer.client.cancel, deadline, async {
                sink.write_all(&chunk.bytes)
                    .await
                    .map_err(|error| Error::LocalIo(error.kind()))
            })
            .await;
            match written {
                Err(error @ Error::LocalIo(_)) => {
                    return Err(transfer.abort(&handle, error, deadline).await);
                }
                other => other?,
            }
            offset += chunk.bytes.len() as u64;
            if chunk.eof {
                break;
            }
        }
        transfer.client.close(&handle).await?;
        transfer.client.ready()?;
        let flushed = bounded(&transfer.client.cancel, async {
            sink.flush()
                .await
                .map_err(|error| Error::LocalIo(error.kind()))
        })
        .await;
        if let Err(error @ Error::LocalIo(_)) = flushed {
            // Close was already acknowledged. Do not issue a second Close or
            // discard a healthy channel for a returned local flush failure.
            transfer.client.ready()?;
            transfer.finish();
            return Err(error);
        }
        flushed?;
        transfer.client.ready()?;
        transfer.finish();
        Ok(size)
    }
    /// Reads exactly `size` bytes followed by EOF. No automatic finish replay,
    /// replacement decision, local path access or whole-file buffering occurs.
    pub async fn upload(
        &mut self,
        value: &str,
        size: u64,
        expected: Option<&Handle>,
        source: &mut (impl AsyncRead + Unpin),
    ) -> Result<u64> {
        // The exchange itself closes uncertain/dropped admission. A fully
        // received pre-admission refusal has created no upload and must leave
        // the source unread so the caller can obtain a fresh collision review.
        let handle = self.begin_upload(value, size, expected).await?;
        let transfer = Transfer {
            client: self,
            complete: false,
        };
        let mut bytes = vec![0; transfer.client.capabilities.chunk_limit];
        let mut offset = 0;
        while offset < size {
            transfer.client.ready()?;
            let length = (size - offset).min(bytes.len() as u64) as usize;
            let deadline = tokio::time::Instant::now() + DEADLINE;
            let read = bounded_until(&transfer.client.cancel, deadline, async {
                source
                    .read(&mut bytes[..length])
                    .await
                    .map_err(|error| Error::LocalIo(error.kind()))
            })
            .await;
            let read = match read {
                Err(error @ Error::LocalIo(_)) => {
                    return Err(transfer.abort(&handle, error, deadline).await);
                }
                other => other?,
            };
            if read == 0 {
                return Err(Error::Invalid);
            }
            offset = transfer.client.write(&handle, &bytes[..read]).await?;
        }
        transfer.client.ready()?;
        let deadline = tokio::time::Instant::now() + DEADLINE;
        let extra = bounded_until(&transfer.client.cancel, deadline, async {
            source
                .read(&mut bytes[..1])
                .await
                .map_err(|error| Error::LocalIo(error.kind()))
        })
        .await;
        let extra = match extra {
            Err(error @ Error::LocalIo(_)) => {
                return Err(transfer.abort(&handle, error, deadline).await);
            }
            other => other?,
        };
        if extra != 0 {
            return Err(Error::Invalid);
        }
        match transfer.client.finish_upload(&handle).await {
            Ok(size) => {
                transfer.finish();
                Ok(size)
            }
            Err(
                error @ Error::Remote {
                    code: rpc::ErrorCode::Exists | rpc::ErrorCode::Conflict,
                    ..
                },
            ) => {
                // The server has consumed the upload handle and returned a
                // completed publication refusal. Retain the channel for a new
                // review, never replay Finish. Staging cleanup is best effort.
                transfer.finish();
                Err(error)
            }
            Err(error) => Err(error),
        }
    }
}
// Also abort on a dropped high-level transfer future while local I/O is pending.
struct Transfer<'a, S> {
    client: &'a mut Client<S>,
    complete: bool,
}
impl<S> Transfer<'_, S> {
    fn finish(mut self) {
        self.complete = true;
    }
}
impl<S: AsyncRead + AsyncWrite + Unpin> Transfer<'_, S> {
    async fn abort(
        mut self,
        handle: &Handle,
        operation: Error,
        deadline: tokio::time::Instant,
    ) -> Error {
        // Local I/O returned between complete wire exchanges. Abort that exact
        // handle once within the original local-I/O deadline. An acknowledged
        // Close consumes the handle and attempts server staging cleanup; it is
        // not a universal unlink guarantee. Unknown/drop paths still fence the
        // entire stream through Transfer::drop.
        let cleanup = if tokio::time::Instant::now() >= deadline {
            Err(Error::Deadline)
        } else {
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(deadline) => Err(Error::Deadline),
                result = self.client.close(handle) => {
                    // A task can resume late with an already-readable reply.
                    // Observing it after the original deadline never permits
                    // retaining the stream, even if Close itself returned Ok.
                    if tokio::time::Instant::now() >= deadline {
                        Err(Error::Deadline)
                    } else {
                        result
                    }
                }
            }
        };
        match cleanup {
            Ok(()) => {
                self.complete = true;
                operation
            }
            Err(cleanup) => Error::CleanupFailed {
                operation: Box::new(operation),
                cleanup: Box::new(cleanup),
            },
        }
    }
}
impl<S> Drop for Transfer<'_, S> {
    fn drop(&mut self) {
        if !self.complete {
            self.client.stream = None;
            self.client.handles.clear();
        }
    }
}

#[cfg(all(test, feature = "relay"))]
mod tests {
    use super::*;
    use crate::{files::FileService, transport};
    use tokio::io::DuplexStream;

    fn selected() -> (festerm_sessiond::NativeSessionIdentity, Target) {
        let identity = festerm_sessiond::NativeSessionIdentity {
            name: "rooted-client-no-daemon".into(),
            pid: std::process::id(),
            created_at_unix_ms: 123,
            endpoint: "unused-daemon-endpoint".into(),
        };
        let target = Target {
            name: identity.name.clone(),
            pid: identity.pid,
            generation: "123".into(),
        };
        (identity, target)
    }
    fn service(
        files: FileService,
    ) -> (
        DuplexStream,
        tokio::task::JoinHandle<io::Result<()>>,
        Target,
    ) {
        let (stream, server) = tokio::io::duplex(8192);
        let (identity, target) = selected();
        let task =
            tokio::spawn(async move { transport::serve_with_files(server, identity, files).await });
        (stream, task, target)
    }
    fn grant(write: bool) -> (std::path::PathBuf, FileService) {
        let root = crate::test_support::private_directory("festerm-rooted-client");
        let files = FileService::rooted(&rpc::Policy {
            delete: false,
            root: root.clone(),
            write,
        })
        .unwrap();
        (root, files)
    }
    async fn stopped(task: tokio::task::JoinHandle<io::Result<()>>) {
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }
    fn limits() -> rpc::Capabilities {
        rpc::Capabilities {
            version: rpc::VERSION,
            write: true,
            chunk_limit: rpc::CHUNK_LIMIT,
            page_limit: rpc::PAGE_LIMIT,
            handle_limit: rpc::HANDLE_LIMIT,
        }
    }

    #[cfg(feature = "testing")]
    #[tokio::test]
    async fn boundary_probes_reach_real_read_only_service_and_preserve_owned_files() {
        let (root, files) = grant(false);
        let leaf = "festerm-boundary-0123456789abcdef";
        std::fs::create_dir(root.join(leaf)).unwrap();
        std::fs::write(root.join(leaf).join("canary.txt"), b"public canary").unwrap();
        let (stream, server, target) = service(files);
        let mut client = Client::open(stream, target, CancellationToken::new())
            .await
            .unwrap();
        let (read, _) = client
            .open_read(&format!("/{leaf}/canary.txt"))
            .await
            .unwrap();
        assert_eq!(
            client.read(&read, 0, 64).await.unwrap().bytes,
            b"public canary"
        );
        client.close(&read).await.unwrap();
        for (probe, expected) in [
            (BoundaryProbe::ParentTraversal, rpc::ErrorCode::Invalid),
            (BoundaryProbe::Backslash, rpc::ErrorCode::Invalid),
            (BoundaryProbe::DrivePath, rpc::ErrorCode::Invalid),
            (BoundaryProbe::ReservedName, rpc::ErrorCode::Invalid),
            (BoundaryProbe::ReadOnlyMkdir, rpc::ErrorCode::Denied),
            (BoundaryProbe::ReadOnlyUpload, rpc::ErrorCode::Denied),
        ] {
            let before = client.next_id;
            assert_eq!(
                client.probe_boundary_refusal(probe, leaf).await.unwrap(),
                expected
            );
            assert_eq!(
                client.next_id,
                before + 1,
                "one correlated wire request, never a local denial"
            );
        }
        let before = client.next_id;
        for invalid in [
            "unowned",
            "festerm-boundary-../../outside",
            "festerm-boundary-0123456789abcdef/child",
        ] {
            assert!(matches!(
                client
                    .probe_boundary_refusal(BoundaryProbe::ParentTraversal, invalid)
                    .await,
                Err(Error::Invalid)
            ));
        }
        assert_eq!(
            client.next_id, before,
            "invalid ownership labels send no request"
        );
        client.ping().await.unwrap();
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(root.join(leaf)).unwrap().count(), 1);
        assert_eq!(
            std::fs::read(root.join(leaf).join("canary.txt")).unwrap(),
            b"public canary"
        );
        client.cancel();
        stopped(server).await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(feature = "testing")]
    #[tokio::test]
    async fn boundary_probe_account_or_request_cancellation_sends_no_request() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for account_cutover in [false, true] {
            let scope = rpc::HostScope {
                host: Default::default(),
                installation: Default::default(),
                epoch: "29".into(),
            };
            let expected = scope.clone();
            let (stream, mut peer) = tokio::io::duplex(8192);
            let server = tokio::spawn(async move {
                scripted_host_open(&mut peer, &expected).await;
                assert!(crate::wire::read_frame(&mut peer).await.is_err());
            });
            let current = Arc::new(AtomicBool::new(true));
            let checked = current.clone();
            let guard = crate::host_client::ordinary::OrdinaryAccountGuard::new(
                CancellationToken::new(),
                Arc::new(move || {
                    if checked.load(Ordering::SeqCst) {
                        Ok(())
                    } else {
                        Err(festerm_host_protocol::ContractError::Stale)
                    }
                }),
            );
            let cancel = CancellationToken::new();
            let (mut client, _) = Client::open_host_guarded(
                stream,
                &scope.host,
                &scope.installation,
                None,
                guard,
                cancel.clone(),
            )
            .await
            .unwrap()
            .unwrap();
            if account_cutover {
                current.store(false, Ordering::SeqCst);
            } else {
                cancel.cancel();
            }
            assert!(matches!(
                client
                    .probe_boundary_refusal(
                        BoundaryProbe::ParentTraversal,
                        "festerm-boundary-0123456789abcdef"
                    )
                    .await,
                Err(Error::Cancelled)
            ));
            assert_eq!(client.next_id, 0);
            server.await.unwrap();
        }
    }

    #[cfg(feature = "testing")]
    #[tokio::test(start_paused = true)]
    async fn boundary_probe_uses_existing_deadline_without_replay() {
        let scope = rpc::HostScope {
            host: Default::default(),
            installation: Default::default(),
            epoch: "29".into(),
        };
        let expected = scope.clone();
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            scripted_host_open(&mut peer, &expected).await;
            let request = request(&mut peer).await;
            assert!(
                matches!(request.operation, Operation::OpenRead { path } if path == "/../festerm-boundary-0123456789abcdef/outside.txt")
            );
            assert!(
                crate::wire::read_frame(&mut peer).await.is_err(),
                "no retry or Finish after the original deadline"
            );
        });
        let (mut client, _) = Client::open_host(
            stream,
            &scope.host,
            &scope.installation,
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
        let started = tokio::time::Instant::now();
        assert!(matches!(
            client
                .probe_boundary_refusal(
                    BoundaryProbe::ParentTraversal,
                    "festerm-boundary-0123456789abcdef"
                )
                .await,
            Err(Error::Deadline)
        ));
        assert_eq!(started.elapsed(), DEADLINE);
        assert!(!client.available());
        assert_eq!(client.next_id, 1);
        server.await.unwrap();
    }

    #[cfg(feature = "testing")]
    #[tokio::test]
    async fn boundary_probe_never_accepts_success_or_issues_followup_after_it() {
        let scope = rpc::HostScope {
            host: Default::default(),
            installation: Default::default(),
            epoch: "29".into(),
        };
        let expected = scope.clone();
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            scripted_host_open(&mut peer, &expected).await;
            let request = request(&mut peer).await;
            assert!(matches!(request.operation, Operation::OpenRead { .. }));
            reply(&mut peer, request.id, Reply::Created).await;
            assert!(crate::wire::read_frame(&mut peer).await.is_err());
        });
        let (mut client, _) = Client::open_host(
            stream,
            &scope.host,
            &scope.installation,
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(
            client
                .probe_boundary_refusal(
                    BoundaryProbe::ParentTraversal,
                    "festerm-boundary-0123456789abcdef"
                )
                .await,
            Err(Error::Protocol)
        ));
        assert!(!client.available());
        assert_eq!(client.next_id, 1);
        server.await.unwrap();
    }

    #[cfg(all(feature = "testing", unix))]
    #[tokio::test]
    async fn boundary_write_link_upload_reaches_real_service_without_publication() {
        let (root, files) = grant(true);
        let outside = crate::test_support::private_directory("festerm-boundary-outside");
        let leaf = "festerm-boundary-0123456789abcdef";
        std::fs::create_dir(root.join(leaf)).unwrap();
        std::fs::write(root.join(leaf).join("canary.txt"), b"public canary").unwrap();
        std::fs::write(outside.join("outside.txt"), b"outside canary").unwrap();
        std::os::unix::fs::symlink(&outside, root.join(leaf).join("escape")).unwrap();
        let (stream, server, target) = service(files);
        let mut client = Client::open(stream, target, CancellationToken::new())
            .await
            .unwrap();
        let before = client.next_id;
        assert!(matches!(
            client
                .begin_upload(&format!("/{leaf}/escape/denied-upload"), 0, None)
                .await,
            Err(Error::Remote {
                code: rpc::ErrorCode::Denied | rpc::ErrorCode::Invalid | rpc::ErrorCode::Io,
                ..
            })
        ));
        assert_eq!(
            client.next_id,
            before + 1,
            "valid path reached the real server once"
        );
        client.ping().await.unwrap();
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
        assert_eq!(
            std::fs::read(outside.join("outside.txt")).unwrap(),
            b"outside canary"
        );
        assert_eq!(
            std::fs::read(root.join(leaf).join("canary.txt")).unwrap(),
            b"public canary"
        );
        client.cancel();
        stopped(server).await;
        // Remove the owned link itself, never recurse through its destination.
        std::fs::remove_file(root.join(leaf).join("escape")).unwrap();
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(feature = "testing")]
    #[tokio::test]
    async fn boundary_write_delete_off_reaches_reviewed_service_and_preserves_canary() {
        let root = crate::test_support::private_directory("festerm-boundary-reviewed-off");
        let leaf = "festerm-boundary-0123456789abcdef";
        std::fs::create_dir(root.join(leaf)).unwrap();
        std::fs::write(root.join(leaf).join("canary.txt"), b"public canary").unwrap();
        let filesystem = crate::filesystem::Filesystem::open(&rpc::Policy {
            root: root.clone(),
            write: true,
            delete: false,
        })
        .unwrap();
        let scope = reviewed_scope();
        let expected = scope.clone();
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            write_frame(
                &mut peer,
                rpc::HOST_DESCRIPTION,
                &json(&rpc::HostDescription {
                    version: rpc::HOST_VERSION,
                    scope: expected.clone(),
                    filesystem: Some(filesystem.capabilities()),
                })
                .unwrap(),
            )
            .await?;
            let (kind, bytes) = crate::wire::read_frame(&mut peer).await?;
            assert_eq!(kind, rpc::OPEN);
            let open: rpc::HostOpen = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(open.version, rpc::HOST_REVIEWED_VERSION);
            assert_eq!(open.scope, expected);
            crate::filesystem::serve_authorized_reviewed(
                peer,
                filesystem,
                Arc::new(|_| Ok(())),
                CancellationToken::new(),
            )
            .await
        });
        let (mut client, _) = Client::open_host_reviewed(
            stream,
            &scope.host,
            &scope.installation,
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(client.capabilities().write);
        assert!(!client.delete_advertised());
        let before = client.next_id;
        assert_eq!(
            client.probe_delete_off_refusal(leaf).await.unwrap(),
            rpc::ErrorCode::Denied
        );
        assert_eq!(
            client.next_id,
            before + 2,
            "fresh Stat then exactly one review preparation"
        );
        let before = client.next_id;
        assert!(matches!(
            client.probe_delete_off_refusal("unowned").await,
            Err(Error::Invalid)
        ));
        assert_eq!(client.next_id, before);
        client.ping().await.unwrap();
        assert_eq!(std::fs::read_dir(root.join(leaf)).unwrap().count(), 1);
        assert_eq!(
            std::fs::read(root.join(leaf).join("canary.txt")).unwrap(),
            b"public canary"
        );
        client.cancel();
        stopped(server).await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(feature = "testing")]
    fn boundary_canary_metadata() -> rpc::Metadata {
        rpc::Metadata {
            kind: rpc::Kind::File,
            size: "31".into(),
            modified_ms: None,
        }
    }

    #[cfg(feature = "testing")]
    #[tokio::test]
    async fn boundary_write_probe_wrong_grant_sends_no_stat_or_review() {
        let (root, files) = grant(false);
        let (stream, server, target) = service(files);
        let mut client = Client::open(stream, target, CancellationToken::new())
            .await
            .unwrap();
        assert!(!client.capabilities().write);
        assert!(matches!(
            client
                .probe_delete_off_refusal("festerm-boundary-0123456789abcdef")
                .await,
            Err(Error::Invalid)
        ));
        assert_eq!(client.next_id, 0);
        client.ping().await.unwrap();
        client.cancel();
        stopped(server).await;
        std::fs::remove_dir_all(root).unwrap();

        let (root, stream, server, scope) =
            native_reviewed_service(rpc::HOST_REVIEWED_VERSION, false);
        let (mut client, _) = Client::open_host_reviewed(
            stream,
            &scope.host,
            &scope.installation,
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(client.capabilities().write && client.delete_advertised());
        assert!(matches!(
            client
                .probe_delete_off_refusal("festerm-boundary-0123456789abcdef")
                .await,
            Err(Error::Invalid)
        ));
        assert_eq!(client.next_id, 0);
        client.ping().await.unwrap();
        assert_eq!(std::fs::read(root.join("file")).unwrap(), b"owned source");
        client.cancel();
        stopped(server).await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(feature = "testing")]
    #[tokio::test]
    async fn boundary_write_probe_cancellation_fences_stat_and_review() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for cutover_after_stat in [false, true] {
            for account_cutover in [false, true] {
                let scope = reviewed_scope();
                let expected = scope.clone();
                let (stream, mut peer) = tokio::io::duplex(8192);
                let current = Arc::new(AtomicBool::new(true));
                let checked = current.clone();
                let current_control = current.clone();
                let cancel = CancellationToken::new();
                let cancelled = cancel.clone();
                let server = tokio::spawn(async move {
                    scripted_host_open(&mut peer, &expected).await;
                    if cutover_after_stat {
                        let stat = request(&mut peer).await;
                        assert!(
                            matches!(stat.operation, Operation::Stat { path } if path == "/festerm-boundary-0123456789abcdef/canary.txt")
                        );
                        if account_cutover {
                            current.store(false, Ordering::SeqCst);
                        } else {
                            cancelled.cancel();
                        }
                        // The client may already have retired after cancellation.
                        let _ = write_frame(
                            &mut peer,
                            rpc::RESPONSE,
                            &json(&rpc::Response {
                                id: stat.id,
                                result: Some(Reply::Stat {
                                    metadata: boundary_canary_metadata(),
                                }),
                                error: None,
                                recovery_path: None,
                            })
                            .unwrap(),
                        )
                        .await;
                    }
                    assert!(
                        crate::wire::read_frame(&mut peer).await.is_err(),
                        "no review or Delete after cancellation"
                    );
                });
                let guard = crate::host_client::ordinary::OrdinaryAccountGuard::new(
                    CancellationToken::new(),
                    Arc::new(move || {
                        if checked.load(Ordering::SeqCst) {
                            Ok(())
                        } else {
                            Err(festerm_host_protocol::ContractError::Stale)
                        }
                    }),
                );
                let (mut client, _) = Client::open_host_guarded(
                    stream,
                    &scope.host,
                    &scope.installation,
                    None,
                    guard,
                    cancel.clone(),
                )
                .await
                .unwrap()
                .unwrap();
                if !cutover_after_stat {
                    if account_cutover {
                        /* same captured guard, no credential fallback */
                        current_control.store(false, Ordering::SeqCst);
                    } else {
                        cancel.cancel();
                    }
                }
                assert!(matches!(
                    client
                        .probe_delete_off_refusal("festerm-boundary-0123456789abcdef")
                        .await,
                    Err(Error::Cancelled)
                ));
                assert_eq!(client.next_id, u32::from(cutover_after_stat));
                server.await.unwrap();
            }
        }
    }

    #[cfg(feature = "testing")]
    #[tokio::test(start_paused = true)]
    async fn boundary_write_probe_deadline_and_unexpected_review_never_send_followup() {
        for accept in [false, true] {
            let scope = reviewed_scope();
            let expected = scope.clone();
            let (stream, mut peer) = tokio::io::duplex(8192);
            let server = tokio::spawn(async move {
                scripted_host_open(&mut peer, &expected).await;
                let stat = request(&mut peer).await;
                assert!(matches!(stat.operation, Operation::Stat { .. }));
                reply(
                    &mut peer,
                    stat.id,
                    Reply::Stat {
                        metadata: boundary_canary_metadata(),
                    },
                )
                .await;
                let review = request(&mut peer).await;
                assert!(
                    matches!(review.operation, Operation::ReviewEntry { path, purpose: rpc::ReviewPurpose::Delete } if path == "/festerm-boundary-0123456789abcdef/canary.txt")
                );
                if accept {
                    reply(
                        &mut peer,
                        review.id,
                        Reply::EntryReview {
                            handle: 1,
                            metadata: boundary_canary_metadata(),
                        },
                    )
                    .await;
                }
                assert!(
                    crate::wire::read_frame(&mut peer).await.is_err(),
                    "no Delete/Close/retry after acceptance or deadline"
                );
            });
            let (mut client, _) = Client::open_host(
                stream,
                &scope.host,
                &scope.installation,
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .unwrap();
            let started = tokio::time::Instant::now();
            let result = client
                .probe_delete_off_refusal("festerm-boundary-0123456789abcdef")
                .await;
            if accept {
                assert!(matches!(result, Err(Error::Protocol)));
            } else {
                assert!(matches!(result, Err(Error::Deadline)));
                assert_eq!(started.elapsed(), DEADLINE);
            }
            assert_eq!(client.next_id, 2);
            assert!(!client.available());
            server.await.unwrap();
        }
    }
    async fn reply(stream: &mut DuplexStream, id: u32, result: Reply) {
        write_frame(
            stream,
            rpc::RESPONSE,
            &json(&rpc::Response {
                id,
                result: Some(result),
                error: None,
                recovery_path: None,
            })
            .unwrap(),
        )
        .await
        .unwrap();
    }
    async fn request(stream: &mut DuplexStream) -> rpc::Request {
        let (kind, bytes) = crate::wire::read_frame(stream).await.unwrap();
        assert_eq!(kind, rpc::REQUEST);
        serde_json::from_slice(&bytes).unwrap()
    }
    async fn scripted_open(stream: &mut DuplexStream) {
        let (kind, _) = crate::wire::read_frame(stream).await.unwrap();
        assert_eq!(kind, rpc::OPEN);
        reply(
            stream,
            0,
            Reply::Ready {
                capabilities: limits(),
            },
        )
        .await;
    }

    #[cfg(feature = "host-routing")]
    async fn scripted_host_open(stream: &mut DuplexStream, scope: &rpc::HostScope) {
        write_frame(
            stream,
            rpc::HOST_DESCRIPTION,
            &json(&rpc::HostDescription {
                version: rpc::HOST_VERSION,
                scope: scope.clone(),
                filesystem: Some(limits()),
            })
            .unwrap(),
        )
        .await
        .unwrap();
        let (kind, bytes) = crate::wire::read_frame(stream).await.unwrap();
        assert_eq!(kind, rpc::OPEN);
        let open: rpc::HostOpen = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(open.scope, *scope);
        reply(
            stream,
            0,
            Reply::Ready {
                capabilities: limits(),
            },
        )
        .await;
    }

    #[cfg(feature = "host-routing")]
    #[tokio::test]
    async fn ordinary_files_client_account_cutover_prevents_upload_chunks_and_finish() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct CutoverRead {
            current: Arc<AtomicBool>,
            eof: bool,
            delivered: bool,
        }
        impl AsyncRead for CutoverRead {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                buffer: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<io::Result<()>> {
                if !self.delivered {
                    buffer.put_slice(b"x");
                    self.delivered = true;
                    if !self.eof {
                        self.current.store(false, Ordering::SeqCst);
                    }
                } else {
                    self.current.store(false, Ordering::SeqCst);
                }
                std::task::Poll::Ready(Ok(()))
            }
        }
        for after_chunk in [false, true] {
            let scope = rpc::HostScope {
                host: Default::default(),
                installation: Default::default(),
                epoch: "29".into(),
            };
            let expected = scope.clone();
            let (stream, mut peer) = tokio::io::duplex(8192);
            let server = tokio::spawn(async move {
                scripted_host_open(&mut peer, &expected).await;
                let begin = request(&mut peer).await;
                assert!(matches!(begin.operation, Operation::BeginUpload { .. }));
                reply(&mut peer, begin.id, Reply::Upload { handle: 1 }).await;
                if after_chunk {
                    let write = request(&mut peer).await;
                    assert!(matches!(write.operation, Operation::Write { .. }));
                    let (kind, _) = crate::wire::read_frame(&mut peer).await.unwrap();
                    assert_eq!(kind, rpc::DATA);
                    reply(&mut peer, write.id, Reply::Written { offset: "1".into() }).await;
                }
                // Never another chunk or FinishUpload after account loss.
                assert!(crate::wire::read_frame(&mut peer).await.is_err());
            });
            let current = Arc::new(AtomicBool::new(true));
            let checked = current.clone();
            let guard = crate::host_client::ordinary::OrdinaryAccountGuard::new(
                CancellationToken::new(),
                Arc::new(move || {
                    if checked.load(Ordering::SeqCst) {
                        Ok(())
                    } else {
                        Err(festerm_host_protocol::ContractError::Stale)
                    }
                }),
            );
            let (mut client, _) = Client::open_host_guarded(
                stream,
                &scope.host,
                &scope.installation,
                None,
                guard,
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .unwrap();
            let mut source = CutoverRead {
                current,
                eof: after_chunk,
                delivered: false,
            };
            assert!(matches!(
                client.upload("/private", 1, None, &mut source).await,
                Err(Error::Cancelled)
            ));
            assert!(!client.available());
            server.await.unwrap();
        }
    }

    #[cfg(feature = "host-routing")]
    #[tokio::test]
    async fn ordinary_files_client_late_account_cutover_keeps_finish_uncertain_without_replay() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let scope = rpc::HostScope {
            host: Default::default(),
            installation: Default::default(),
            epoch: "29".into(),
        };
        let expected = scope.clone();
        let (stream, mut peer) = tokio::io::duplex(8192);
        let current = Arc::new(AtomicBool::new(true));
        let changed = current.clone();
        let server = tokio::spawn(async move {
            scripted_host_open(&mut peer, &expected).await;
            let begin = request(&mut peer).await;
            reply(&mut peer, begin.id, Reply::Upload { handle: 1 }).await;
            let finish = request(&mut peer).await;
            assert!(matches!(finish.operation, Operation::FinishUpload { .. }));
            changed.store(false, Ordering::SeqCst);
            reply(&mut peer, finish.id, Reply::Finished { size: "0".into() }).await;
            assert!(crate::wire::read_frame(&mut peer).await.is_err());
        });
        let guard = crate::host_client::ordinary::OrdinaryAccountGuard::new(
            CancellationToken::new(),
            Arc::new(move || {
                if current.load(Ordering::SeqCst) {
                    Ok(())
                } else {
                    Err(festerm_host_protocol::ContractError::Stale)
                }
            }),
        );
        let (mut client, _) = Client::open_host_guarded(
            stream,
            &scope.host,
            &scope.installation,
            None,
            guard,
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
        let handle = client.begin_upload("/empty", 0, None).await.unwrap();
        assert!(matches!(
            client.finish_upload(&handle).await,
            Err(Error::UncertainFinish(_))
        ));
        assert!(client.finish_upload(&handle).await.is_err());
        assert!(!client.available());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn rooted_client_negotiates_disabled_readonly_and_exact_target_without_attachment() {
        let (stream, server, target) = service(FileService::new(false));
        assert!(Client::discover(stream, &target, &CancellationToken::new())
            .await
            .unwrap()
            .is_none());
        server.await.unwrap().unwrap();
        let (stream, server, target) = service(FileService::new(false));
        assert!(Client::open(stream, target, CancellationToken::new())
            .await
            .is_err());
        stopped(server).await;
        let (root, files) = grant(false);
        std::fs::write(root.join("source"), b"read only").unwrap();
        let (stream, server, target) = service(files.clone());
        let discovered = Client::discover(stream, &target, &CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        assert!(!discovered.write);
        server.await.unwrap().unwrap();
        let (stream, server, target) = service(files.clone());
        let mut client = Client::open(stream, target, CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(
            client.begin_upload("/new", 0, None).await,
            Err(Error::Invalid)
        ));
        assert!(matches!(client.mkdir("/new").await, Err(Error::Invalid)));
        let mut downloaded = Vec::new();
        assert_eq!(
            client.download("/source", &mut downloaded).await.unwrap(),
            9
        );
        assert_eq!(downloaded, b"read only");
        client.cancel();
        stopped(server).await;
        let (stream, server, mut target) = service(files);
        target.generation = "124".into();
        assert!(Client::open(stream, target, CancellationToken::new())
            .await
            .is_err());
        stopped(server).await;
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn rooted_client_streams_pages_large_files_and_explicitly_releases_handles() {
        let (root, files) = grant(true);
        let content: Vec<u8> = (0..rpc::CHUNK_LIMIT * 3 + 17)
            .map(|n| (n % 251) as u8)
            .collect();
        std::fs::write(root.join("source"), &content).unwrap();
        let (stream, server, target) = service(files);
        let mut client = Client::open(stream, target, CancellationToken::new())
            .await
            .unwrap();
        let directory = client.open_directory("/").await.unwrap();
        let mut names = Vec::new();
        loop {
            let page = client.read_directory(&directory, 1).await.unwrap();
            names.extend(page.entries.into_iter().map(|entry| entry.name));
            if page.done {
                break;
            }
        }
        assert_eq!(names, ["source"]);
        client.close(&directory).await.unwrap();
        assert!(matches!(
            client.read_directory(&directory, 1).await,
            Err(Error::Invalid)
        ));
        let mut handles = Vec::new();
        for _ in 0..rpc::HANDLE_LIMIT {
            handles.push(client.open_read("/source").await.unwrap().0);
        }
        assert!(matches!(
            client.open_read("/source").await,
            Err(Error::Invalid)
        ));
        client.close(&handles.pop().unwrap()).await.unwrap();
        let handle = client.open_read("/source").await.unwrap().0;
        assert!(matches!(
            client.read(&handle, 0, rpc::CHUNK_LIMIT as u32 + 1).await,
            Err(Error::Invalid)
        ));
        let eof = client.read(&handle, content.len() as u64, 1).await.unwrap();
        assert!(eof.eof && eof.bytes.is_empty());
        assert_eq!(
            client.handles.len(),
            rpc::HANDLE_LIMIT,
            "EOF must not release the handle"
        );
        client.close(&handle).await.unwrap();
        for handle in handles {
            client.close(&handle).await.unwrap();
        }
        let mut downloaded = Vec::new();
        assert_eq!(
            client.download("/source", &mut downloaded).await.unwrap(),
            content.len() as u64
        );
        assert_eq!(downloaded, content);
        assert!(client.handles.is_empty());
        let mut source = content.as_slice();
        client
            .upload("/uploaded", content.len() as u64, None, &mut source)
            .await
            .unwrap();
        assert_eq!(std::fs::read(root.join("uploaded")).unwrap(), content);
        assert!(client.handles.is_empty());
        // Replacement is explicit and names a currently open version handle.
        let original = client.open_read("/uploaded").await.unwrap().0;
        let mut replacement = &b"replacement"[..];
        client
            .upload("/uploaded", 11, Some(&original), &mut replacement)
            .await
            .unwrap();
        client.close(&original).await.unwrap();
        assert_eq!(
            std::fs::read(root.join("uploaded")).unwrap(),
            b"replacement"
        );
        let abandoned = client.begin_upload("/aborted", 4, None).await.unwrap();
        client.write(&abandoned, b"part").await.unwrap();
        client.close(&abandoned).await.unwrap();
        assert!(!root.join("aborted").exists());
        client.cancel();
        stopped(server).await;
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn rooted_client_handles_are_connection_local_and_ids_never_wrap() {
        let (root, files) = grant(true);
        std::fs::write(root.join("source"), b"a").unwrap();
        let (stream, first_server, target) = service(files.clone());
        let mut first = Client::open(stream, target, CancellationToken::new())
            .await
            .unwrap();
        let handle = first.open_read("/source").await.unwrap().0;
        let (stream, second_server, target) = service(files);
        let mut second = Client::open(stream, target, CancellationToken::new())
            .await
            .unwrap();
        let own = second.open_read("/source").await.unwrap().0;
        assert_eq!(handle.id, own.id);
        assert!(matches!(
            second.read(&handle, 0, 1).await,
            Err(Error::Invalid)
        ));
        assert_eq!(second.read(&own, 0, 1).await.unwrap().bytes, b"a");
        second.next_id = u32::MAX;
        assert!(matches!(second.stat("/source").await, Err(Error::Protocol)));
        assert!(!second.available());
        stopped(second_server).await;
        first.cancel();
        stopped(first_server).await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn rooted_client_refuses_malformed_bounds_ids_and_data_before_reuse() {
        for fault in 0..5 {
            let (stream, mut peer) = tokio::io::duplex(8192);
            let server = tokio::spawn(async move {
                let (kind, _) = crate::wire::read_frame(&mut peer).await.unwrap();
                assert_eq!(kind, rpc::OPEN);
                let mut caps = limits();
                if fault == 0 {
                    caps.chunk_limit = rpc::CHUNK_LIMIT + 1;
                }
                reply(&mut peer, 0, Reply::Ready { capabilities: caps }).await;
                if fault == 0 {
                    return;
                }
                let call = request(&mut peer).await;
                if fault == 1 {
                    reply(
                        &mut peer,
                        call.id + 1,
                        Reply::File {
                            handle: 1,
                            metadata: rpc::Metadata {
                                kind: rpc::Kind::File,
                                size: "2".into(),
                                modified_ms: None,
                            },
                        },
                    )
                    .await;
                } else if fault == 2 {
                    // Header alone must refuse: no oversized payload is sent.
                    peer.write_all(&[rpc::RESPONSE]).await.unwrap();
                    peer.write_all(&(rpc::JSON_LIMIT as u32 + 1).to_be_bytes())
                        .await
                        .unwrap();
                    std::future::pending::<()>().await;
                } else {
                    reply(
                        &mut peer,
                        call.id,
                        Reply::File {
                            handle: 1,
                            metadata: rpc::Metadata {
                                kind: rpc::Kind::File,
                                size: "2".into(),
                                modified_ms: None,
                            },
                        },
                    )
                    .await;
                    let call = request(&mut peer).await;
                    if fault == 4 {
                        reply(
                            &mut peer,
                            call.id,
                            Reply::Read {
                                offset: "0".into(),
                                length: rpc::CHUNK_LIMIT as u32 + 1,
                                eof: true,
                            },
                        )
                        .await;
                        return;
                    }
                    reply(
                        &mut peer,
                        call.id,
                        Reply::Read {
                            offset: "0".into(),
                            length: 2,
                            eof: true,
                        },
                    )
                    .await;
                    let mut data = (call.id + 1).to_be_bytes().to_vec();
                    data.extend_from_slice(b"ab");
                    write_frame(&mut peer, rpc::DATA, &data).await.unwrap();
                }
            });
            let (_, target) = selected();
            let opened = Client::open(stream, target, CancellationToken::new()).await;
            if fault == 0 {
                assert!(matches!(opened, Err(Error::Protocol)));
            } else {
                let mut client = opened.unwrap();
                if fault >= 3 {
                    let handle = client.open_read("/source").await.unwrap().0;
                    assert!(matches!(
                        client.read(&handle, 0, 2).await,
                        Err(Error::Protocol)
                    ));
                } else {
                    assert!(matches!(
                        client.open_read("/source").await,
                        Err(Error::Protocol)
                    ));
                }
                assert!(!client.available());
            }
            if fault == 2 {
                server.abort();
            } else {
                server.await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn rooted_client_empty_pages_and_retired_handles_preserve_stream_bounds() {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            scripted_open(&mut peer).await;
            let call = request(&mut peer).await;
            reply(&mut peer, call.id, Reply::Directory { handle: 1 }).await;
            let call = request(&mut peer).await;
            reply(
                &mut peer,
                call.id,
                Reply::Entries {
                    entries: Vec::new(),
                    done: false,
                    skipped: rpc::PAGE_LIMIT * 4,
                },
            )
            .await;
            let call = request(&mut peer).await;
            reply(
                &mut peer,
                call.id,
                Reply::Entries {
                    entries: Vec::new(),
                    done: true,
                    skipped: 0,
                },
            )
            .await;
            let call = request(&mut peer).await;
            assert!(matches!(call.operation, Operation::Close { handle: 1 }));
            reply(&mut peer, call.id, Reply::Closed).await;
            let call = request(&mut peer).await;
            reply(&mut peer, call.id, Reply::Directory { handle: 1 }).await;
            let call = request(&mut peer).await;
            assert!(matches!(call.operation, Operation::Close { handle: 1 }));
            reply(&mut peer, call.id, Reply::Closed).await;
        });
        let (_, target) = selected();
        let mut client = Client::open(stream, target, CancellationToken::new())
            .await
            .unwrap();
        let handle = client.open_directory("/").await.unwrap();
        let page = client.read_directory(&handle, 1).await.unwrap();
        assert!(page.entries.is_empty() && !page.done);
        assert!(client.read_directory(&handle, 1).await.unwrap().done);
        client.close(&handle).await.unwrap();
        let replacement = client.open_directory("/").await.unwrap();
        assert!(matches!(client.close(&handle).await, Err(Error::Invalid)));
        client.close(&replacement).await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn rooted_client_uncertain_finish_closes_without_replay() {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            scripted_open(&mut peer).await;
            let call = request(&mut peer).await;
            assert!(matches!(call.operation, Operation::BeginUpload { .. }));
            reply(&mut peer, call.id, Reply::Upload { handle: 1 }).await;
            let finish = request(&mut peer).await;
            assert!(matches!(
                finish.operation,
                Operation::FinishUpload { handle: 1 }
            ));
            // The peer may have published; deliberately omit the acknowledgement.
            // This proves client uncertainty/no replay, not a host crash outcome.
        });
        let (_, target) = selected();
        let mut client = Client::open(stream, target, CancellationToken::new())
            .await
            .unwrap();
        let handle = client.begin_upload("/empty", 0, None).await.unwrap();
        assert!(matches!(
            client.finish_upload(&handle).await,
            Err(Error::UncertainFinish(_))
        ));
        assert!(!client.available());
        assert!(matches!(
            client.finish_upload(&handle).await,
            Err(Error::Closed)
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn rooted_client_dropped_finish_future_cannot_be_replayed() {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let (received, finish_received) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            scripted_open(&mut peer).await;
            let call = request(&mut peer).await;
            reply(&mut peer, call.id, Reply::Upload { handle: 1 }).await;
            let call = request(&mut peer).await;
            assert!(matches!(
                call.operation,
                Operation::FinishUpload { handle: 1 }
            ));
            received.send(()).unwrap();
            // The next observation must be closure, never another finish.
            assert!(crate::wire::read_frame(&mut peer).await.is_err());
        });
        let (_, target) = selected();
        let mut client = Client::open(stream, target, CancellationToken::new())
            .await
            .unwrap();
        let handle = client.begin_upload("/empty", 0, None).await.unwrap();
        {
            let mut finish = Box::pin(client.finish_upload(&handle));
            tokio::select! {
                result = &mut finish => panic!("finish has no acknowledgement: {result:?}"),
                ready = finish_received => ready.unwrap(),
            }
        }
        assert!(!client.available());
        assert!(matches!(
            client.finish_upload(&handle).await,
            Err(Error::Closed)
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn rooted_client_wrong_source_lengths_abort_without_publication() {
        for bytes in [&b""[..], &b"ab"[..]] {
            let (root, files) = grant(true);
            let (stream, server, target) = service(files);
            let mut client = Client::open(stream, target, CancellationToken::new())
                .await
                .unwrap();
            let mut source = bytes;
            assert!(matches!(
                client.upload("/aborted", 1, None, &mut source).await,
                Err(Error::Invalid)
            ));
            assert!(!client.available());
            stopped(server).await;
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn rooted_client_completed_upload_refusal_keeps_source_unread_and_allows_fresh_review() {
        for code in [rpc::ErrorCode::Exists, rpc::ErrorCode::Conflict] {
            let (stream, mut peer) = tokio::io::duplex(8192);
            let server = tokio::spawn(async move {
                scripted_open(&mut peer).await;
                let begin = request(&mut peer).await;
                assert!(matches!(begin.operation, Operation::BeginUpload { .. }));
                write_frame(
                    &mut peer,
                    rpc::RESPONSE,
                    &json(&rpc::Response {
                        id: begin.id,
                        result: None,
                        error: Some(code),
                        recovery_path: None,
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
                let review = request(&mut peer).await;
                assert!(matches!(review.operation, Operation::OpenRead { .. }));
                reply(
                    &mut peer,
                    review.id,
                    Reply::File {
                        handle: 9,
                        metadata: rpc::Metadata {
                            kind: rpc::Kind::File,
                            size: "1".into(),
                            modified_ms: None,
                        },
                    },
                )
                .await;
                let ping = request(&mut peer).await;
                assert!(matches!(ping.operation, Operation::Ping { .. }));
                reply(&mut peer, ping.id, Reply::Pong).await;
                let close = request(&mut peer).await;
                assert!(matches!(close.operation, Operation::Close { handle: 9 }));
                reply(&mut peer, close.id, Reply::Closed).await;
                let mut eof = [0];
                assert_eq!(peer.read(&mut eof).await.unwrap(), 0);
            });
            let mut client = Client::open(stream, selected().1, CancellationToken::new())
                .await
                .unwrap();
            let mut source = std::io::Cursor::new(b"new bytes");
            assert!(matches!(
                client.upload("/target", 9, None, &mut source).await,
                Err(Error::Remote {
                    code: rpc::ErrorCode::Exists | rpc::ErrorCode::Conflict,
                    ..
                })
            ));
            assert_eq!(source.position(), 0);
            assert!(client.available());
            let (review, metadata) = client.open_read("/target").await.unwrap();
            assert_eq!(metadata.size, "1");
            client.ping().await.unwrap();
            client.close(&review).await.unwrap();
            client.cancel();
            tokio::time::timeout(std::time::Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap();
        }
    }

    #[tokio::test]
    async fn rooted_client_completed_finish_refusal_keeps_stream_and_allows_fresh_review() {
        for code in [rpc::ErrorCode::Exists, rpc::ErrorCode::Conflict] {
            let (stream, mut peer) = tokio::io::duplex(8192);
            let server = tokio::spawn(async move {
                scripted_open(&mut peer).await;
                let begin = request(&mut peer).await;
                assert!(matches!(begin.operation, Operation::BeginUpload { .. }));
                reply(&mut peer, begin.id, Reply::Upload { handle: 1 }).await;
                let write = request(&mut peer).await;
                assert!(matches!(
                    write.operation,
                    Operation::Write { handle: 1, .. }
                ));
                let (kind, bytes) = crate::wire::read_frame(&mut peer).await.unwrap();
                assert_eq!(kind, rpc::DATA);
                assert_eq!(u32::from_be_bytes(bytes[..4].try_into().unwrap()), write.id);
                assert_eq!(&bytes[4..], b"new bytes");
                reply(&mut peer, write.id, Reply::Written { offset: "9".into() }).await;
                let finish = request(&mut peer).await;
                assert!(matches!(
                    finish.operation,
                    Operation::FinishUpload { handle: 1 }
                ));
                write_frame(
                    &mut peer,
                    rpc::RESPONSE,
                    &json(&rpc::Response {
                        id: finish.id,
                        result: None,
                        error: Some(code),
                        recovery_path: None,
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
                let review = request(&mut peer).await;
                assert!(matches!(review.operation, Operation::OpenRead { .. }));
                reply(
                    &mut peer,
                    review.id,
                    Reply::File {
                        handle: 9,
                        metadata: rpc::Metadata {
                            kind: rpc::Kind::File,
                            size: "1".into(),
                            modified_ms: None,
                        },
                    },
                )
                .await;
                let ping = request(&mut peer).await;
                assert!(matches!(ping.operation, Operation::Ping { .. }));
                reply(&mut peer, ping.id, Reply::Pong).await;
                let close = request(&mut peer).await;
                assert!(matches!(close.operation, Operation::Close { handle: 9 }));
                reply(&mut peer, close.id, Reply::Closed).await;
                let mut eof = [0];
                assert_eq!(peer.read(&mut eof).await.unwrap(), 0);
            });
            let mut client = Client::open(stream, selected().1, CancellationToken::new())
                .await
                .unwrap();
            let mut source = std::io::Cursor::new(b"new bytes");
            assert!(matches!(
                client.upload("/target", 9, None, &mut source).await,
                Err(Error::Remote {
                    code: rpc::ErrorCode::Exists | rpc::ErrorCode::Conflict,
                    ..
                })
            ));
            assert_eq!(source.position(), 9);
            assert!(client.available());
            let (review, metadata) = client.open_read("/target").await.unwrap();
            assert_eq!(metadata.size, "1");
            client.ping().await.unwrap();
            client.close(&review).await.unwrap();
            client.cancel();
            tokio::time::timeout(std::time::Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap();
        }
    }

    #[tokio::test]
    async fn rooted_client_dropped_upload_begin_closes_without_reading_source() {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let (sent, received) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            scripted_open(&mut peer).await;
            let begin = request(&mut peer).await;
            assert!(matches!(begin.operation, Operation::BeginUpload { .. }));
            sent.send(()).unwrap();
            let mut eof = [0];
            assert_eq!(peer.read(&mut eof).await.unwrap(), 0);
        });
        let mut client = Client::open(stream, selected().1, CancellationToken::new())
            .await
            .unwrap();
        let mut source = std::io::Cursor::new(b"new bytes");
        {
            let mut upload = Box::pin(client.upload("/target", 9, None, &mut source));
            tokio::select! {
                result = &mut upload => panic!("begin must remain pending: {result:?}"),
                ready = received => ready.unwrap(),
            }
        }
        assert!(!client.available());
        assert_eq!(source.position(), 0);
        tokio::time::timeout(std::time::Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }

    struct InterruptedSource {
        prefix: &'static [u8],
        delay: Option<std::pin::Pin<Box<tokio::time::Sleep>>>,
    }
    impl AsyncRead for InterruptedSource {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buffer: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            if !self.prefix.is_empty() {
                let length = buffer.remaining().min(self.prefix.len());
                buffer.put_slice(&self.prefix[..length]);
                self.prefix = &self.prefix[length..];
                return std::task::Poll::Ready(Ok(()));
            }
            if let Some(delay) = self.delay.as_mut() {
                std::task::ready!(std::future::Future::poll(delay.as_mut(), cx));
            }
            std::task::Poll::Ready(Err(io::ErrorKind::Interrupted.into()))
        }
    }

    #[tokio::test]
    async fn rooted_client_interrupted_upload_closes_handle_and_keeps_stream_usable() {
        let (root, files) = grant(true);
        let (stream, server, target) = service(files);
        let mut client = Client::open(stream, target, CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(
            client
                .upload(
                    "/cancelled",
                    14,
                    None,
                    &mut InterruptedSource {
                        prefix: b"partial",
                        delay: None,
                    }
                )
                .await,
            Err(Error::LocalIo(io::ErrorKind::Interrupted))
        ));
        assert!(
            client.available(),
            "confirmed abort must preserve the stream"
        );
        client.ping().await.unwrap();
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        let mut source = &b"next"[..];
        assert_eq!(
            client.upload("/next", 4, None, &mut source).await.unwrap(),
            4
        );
        assert_eq!(std::fs::read(root.join("next")).unwrap(), b"next");
        client.cancel();
        stopped(server).await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn rooted_client_interrupted_eof_check_aborts_instead_of_finishing() {
        let (root, files) = grant(true);
        let (stream, server, target) = service(files);
        let mut client = Client::open(stream, target, CancellationToken::new())
            .await
            .unwrap();
        let mut source = InterruptedSource {
            prefix: b"a",
            delay: None,
        };
        assert!(matches!(
            client.upload("/not-published", 1, None, &mut source).await,
            Err(Error::LocalIo(io::ErrorKind::Interrupted))
        ));
        client.ping().await.unwrap();
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        client.cancel();
        stopped(server).await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn rooted_client_returned_download_io_failures_preserve_channel() {
        struct InterruptedSink {
            write_fails: bool,
            bytes: Vec<u8>,
        }
        impl AsyncWrite for InterruptedSink {
            fn poll_write(
                mut self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                bytes: &[u8],
            ) -> std::task::Poll<io::Result<usize>> {
                if self.write_fails {
                    return std::task::Poll::Ready(Err(io::ErrorKind::Interrupted.into()));
                }
                self.bytes.extend_from_slice(bytes);
                std::task::Poll::Ready(Ok(bytes.len()))
            }
            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<io::Result<()>> {
                std::task::Poll::Ready(Err(io::ErrorKind::Interrupted.into()))
            }
            fn poll_shutdown(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }
        }
        for write_fails in [true, false] {
            let (root, files) = grant(true);
            std::fs::write(root.join("source"), b"unchanged").unwrap();
            let (stream, server, target) = service(files);
            let mut client = Client::open(stream, target, CancellationToken::new())
                .await
                .unwrap();
            let mut sink = InterruptedSink {
                write_fails,
                bytes: Vec::new(),
            };
            assert!(matches!(
                client.download("/source", &mut sink).await,
                Err(Error::LocalIo(io::ErrorKind::Interrupted))
            ));
            assert!(client.available());
            client.ping().await.unwrap();
            let mut next = Vec::new();
            client.download("/source", &mut next).await.unwrap();
            assert_eq!(next, b"unchanged");
            assert_eq!(std::fs::read(root.join("source")).unwrap(), b"unchanged");
            client.cancel();
            stopped(server).await;
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn rooted_client_failed_abort_preserves_both_errors_and_original_deadline() {
        for withheld in [true, false] {
            let (stream, mut peer) = tokio::io::duplex(8192);
            let server = tokio::spawn(async move {
                scripted_open(&mut peer).await;
                let begin = request(&mut peer).await;
                assert!(matches!(begin.operation, Operation::BeginUpload { .. }));
                reply(&mut peer, begin.id, Reply::Upload { handle: 1 }).await;
                let write = request(&mut peer).await;
                assert!(matches!(write.operation, Operation::Write { .. }));
                let (kind, data) = crate::wire::read_frame(&mut peer).await.unwrap();
                assert_eq!(kind, rpc::DATA);
                assert_eq!(&data[..4], &write.id.to_be_bytes());
                assert_eq!(&data[4..], b"a");
                reply(&mut peer, write.id, Reply::Written { offset: "1".into() }).await;
                let close = request(&mut peer).await;
                assert!(matches!(close.operation, Operation::Close { handle: 1 }));
                if !withheld {
                    reply(&mut peer, close.id, Reply::Pong).await;
                }
                let mut byte = [0];
                assert_eq!(peer.read(&mut byte).await.unwrap(), 0, "no retry or Finish");
            });
            let mut client = Client::open(stream, selected().1, CancellationToken::new())
                .await
                .unwrap();
            let started = tokio::time::Instant::now();
            let mut source = InterruptedSource {
                prefix: b"a",
                delay: Some(Box::pin(tokio::time::sleep(
                    DEADLINE - std::time::Duration::from_secs(1),
                ))),
            };
            let error = client
                .upload("/cancelled", 2, None, &mut source)
                .await
                .unwrap_err();
            match error {
                Error::CleanupFailed { operation, cleanup } => {
                    assert!(matches!(
                        *operation,
                        Error::LocalIo(io::ErrorKind::Interrupted)
                    ));
                    if withheld {
                        assert!(matches!(*cleanup, Error::Deadline));
                        assert_eq!(started.elapsed(), DEADLINE);
                    } else {
                        assert!(matches!(*cleanup, Error::Protocol));
                    }
                }
                other => panic!("both failures required: {other:?}"),
            }
            assert!(!client.available());
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn rooted_client_late_ready_abort_ack_cannot_keep_stream() {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let (seen, received) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let (replied, reply_ready) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            scripted_open(&mut peer).await;
            let begin = request(&mut peer).await;
            assert!(matches!(begin.operation, Operation::BeginUpload { .. }));
            reply(&mut peer, begin.id, Reply::Upload { handle: 1 }).await;
            let close = request(&mut peer).await;
            assert!(matches!(close.operation, Operation::Close { handle: 1 }));
            seen.send(()).unwrap();
            released.await.unwrap();
            reply(&mut peer, close.id, Reply::Closed).await;
            replied.send(()).unwrap();
            let mut byte = [0];
            assert_eq!(peer.read(&mut byte).await.unwrap(), 0);
        });
        let mut client = Client::open(stream, selected().1, CancellationToken::new())
            .await
            .unwrap();
        let mut source = InterruptedSource {
            prefix: b"",
            delay: None,
        };
        let mut upload = Box::pin(client.upload("/cancelled", 1, None, &mut source));
        tokio::select! {
            result = &mut upload => panic!("Close must remain pending: {result:?}"),
            result = received => result.unwrap(),
        }
        release.send(()).unwrap();
        reply_ready.await.unwrap();
        // The response is readable, but the client task is not polled until
        // after its original local-I/O deadline. A late reply cannot win.
        tokio::time::pause();
        tokio::time::advance(DEADLINE + std::time::Duration::from_secs(1)).await;
        assert!(
            matches!(upload.await, Err(Error::CleanupFailed { operation, cleanup })
            if matches!(*operation, Error::LocalIo(io::ErrorKind::Interrupted))
                && matches!(*cleanup, Error::Deadline))
        );
        assert!(!client.available());
        server.await.unwrap();
    }

    #[cfg(feature = "host-routing")]
    #[tokio::test]
    async fn ordinary_files_client_flush_error_fences_late_account_cutover() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct CutoverFlush(Arc<AtomicBool>);
        impl AsyncWrite for CutoverFlush {
            fn poll_write(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                bytes: &[u8],
            ) -> std::task::Poll<io::Result<usize>> {
                std::task::Poll::Ready(Ok(bytes.len()))
            }
            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<io::Result<()>> {
                self.0.store(false, Ordering::SeqCst);
                std::task::Poll::Ready(Err(io::ErrorKind::Interrupted.into()))
            }
            fn poll_shutdown(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }
        }
        let scope = rpc::HostScope {
            host: Default::default(),
            installation: Default::default(),
            epoch: "29".into(),
        };
        let expected = scope.clone();
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            scripted_host_open(&mut peer, &expected).await;
            let open = request(&mut peer).await;
            assert!(matches!(open.operation, Operation::OpenRead { .. }));
            reply(
                &mut peer,
                open.id,
                Reply::File {
                    handle: 1,
                    metadata: rpc::Metadata {
                        kind: rpc::Kind::File,
                        size: "1".into(),
                        modified_ms: None,
                    },
                },
            )
            .await;
            let read = request(&mut peer).await;
            assert!(matches!(read.operation, Operation::Read { handle: 1, .. }));
            reply(
                &mut peer,
                read.id,
                Reply::Read {
                    offset: "0".into(),
                    length: 1,
                    eof: true,
                },
            )
            .await;
            let mut data = read.id.to_be_bytes().to_vec();
            data.push(b'x');
            write_frame(&mut peer, rpc::DATA, &data).await.unwrap();
            let close = request(&mut peer).await;
            assert!(matches!(close.operation, Operation::Close { handle: 1 }));
            reply(&mut peer, close.id, Reply::Closed).await;
            // No second Close or other request after the flush retires its account.
            let mut byte = [0];
            assert_eq!(peer.read(&mut byte).await.unwrap(), 0);
        });
        let current = Arc::new(AtomicBool::new(true));
        let checked = current.clone();
        let guard = crate::host_client::ordinary::OrdinaryAccountGuard::new(
            CancellationToken::new(),
            Arc::new(move || {
                if checked.load(Ordering::SeqCst) {
                    Ok(())
                } else {
                    Err(festerm_host_protocol::ContractError::Stale)
                }
            }),
        );
        let (mut client, _) = Client::open_host_guarded(
            stream,
            &scope.host,
            &scope.installation,
            None,
            guard,
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(
            client.download("/source", &mut CutoverFlush(current)).await,
            Err(Error::Cancelled)
        ));
        assert!(!client.available());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn rooted_client_dropped_or_cancelled_abort_still_fences_stream() {
        for cancel_during_close in [false, true] {
            let (stream, mut peer) = tokio::io::duplex(8192);
            let (seen, received) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                scripted_open(&mut peer).await;
                let begin = request(&mut peer).await;
                assert!(matches!(begin.operation, Operation::BeginUpload { .. }));
                reply(&mut peer, begin.id, Reply::Upload { handle: 1 }).await;
                let close = request(&mut peer).await;
                assert!(matches!(close.operation, Operation::Close { handle: 1 }));
                seen.send(()).unwrap();
                let mut byte = [0];
                assert_eq!(peer.read(&mut byte).await.unwrap(), 0);
            });
            let cancel = CancellationToken::new();
            let mut client = Client::open(stream, selected().1, cancel.clone())
                .await
                .unwrap();
            let mut source = InterruptedSource {
                prefix: b"",
                delay: None,
            };
            {
                let mut upload = Box::pin(client.upload("/cancelled", 1, None, &mut source));
                tokio::select! {
                    result = &mut upload => panic!("Close must remain pending: {result:?}"),
                    result = received => result.unwrap(),
                }
                if cancel_during_close {
                    cancel.cancel();
                    assert!(
                        matches!(upload.await, Err(Error::CleanupFailed { cleanup, .. })
                        if matches!(*cleanup, Error::Cancelled))
                    );
                }
            }
            assert!(!client.available());
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn rooted_client_cancel_and_dropped_local_read_abort_owned_upload() {
        use std::{
            pin::Pin,
            task::{Context, Poll},
        };
        struct WaitingSource(Option<tokio::sync::oneshot::Sender<()>>);
        impl AsyncRead for WaitingSource {
            fn poll_read(
                mut self: Pin<&mut Self>,
                _: &mut Context<'_>,
                _: &mut tokio::io::ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                if let Some(ready) = self.0.take() {
                    let _ = ready.send(());
                }
                Poll::Pending
            }
        }
        for cancel_token in [false, true] {
            let (root, files) = grant(true);
            let (stream, server, target) = service(files);
            let cancel = CancellationToken::new();
            let mut client = Client::open(stream, target, cancel.clone()).await.unwrap();
            let (ready, waiting) = tokio::sync::oneshot::channel();
            let mut source = WaitingSource(Some(ready));
            {
                let mut upload = Box::pin(client.upload("/aborted", 1, None, &mut source));
                tokio::select! {
                    result = &mut upload => panic!("source must remain pending: {result:?}"),
                    ready = waiting => ready.unwrap(),
                }
                if cancel_token {
                    cancel.cancel();
                    assert!(matches!(upload.await, Err(Error::Cancelled)));
                }
            }
            assert!(!client.available());
            stopped(server).await;
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
    async fn reviewed_open(
        peer: &mut DuplexStream,
        scope: &rpc::HostScope,
        operations: Vec<String>,
    ) {
        write_frame(
            peer,
            rpc::HOST_DESCRIPTION,
            &json(&rpc::HostDescription {
                version: rpc::HOST_VERSION,
                scope: scope.clone(),
                filesystem: Some(limits()),
            })
            .unwrap(),
        )
        .await
        .unwrap();
        let (kind, bytes) = crate::wire::read_frame(peer).await.unwrap();
        assert_eq!(kind, rpc::OPEN);
        let open: rpc::HostOpen = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(open.version, rpc::HOST_REVIEWED_VERSION);
        assert_eq!(open.scope, *scope);
        reply(
            peer,
            0,
            Reply::ReadyV3 {
                capabilities: rpc::OperationCapabilities {
                    version: rpc::HOST_REVIEWED_VERSION,
                    write: true,
                    operations,
                    chunk_limit: rpc::CHUNK_LIMIT,
                    page_limit: rpc::PAGE_LIMIT,
                    handle_limit: rpc::HANDLE_LIMIT,
                },
            },
        )
        .await;
    }
    fn reviewed_scope() -> rpc::HostScope {
        rpc::HostScope {
            host: Default::default(),
            installation: Default::default(),
            epoch: "29".into(),
        }
    }
    #[tokio::test]
    async fn reviewed_client_unknown_optional_operations_grant_nothing_and_preserve_ping() {
        let scope = reviewed_scope();
        let expected = scope.clone();
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            reviewed_open(&mut peer, &expected, vec!["future_optional".into()]).await;
            let call = request(&mut peer).await;
            assert!(matches!(call.operation, Operation::Ping {}));
            reply(&mut peer, call.id, Reply::Pong).await;
            let mut eof = [0];
            assert_eq!(peer.read(&mut eof).await.unwrap(), 0);
        });
        let (mut client, _) = Client::open_host_reviewed(
            stream,
            &scope.host,
            &scope.installation,
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!client.delete_advertised());
        assert!(matches!(
            client.review_entry("/file").await,
            Err(Error::Invalid)
        ));
        client.ping().await.unwrap();
        client.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap();
    }
    #[tokio::test]
    async fn reviewed_client_refuses_duplicate_known_and_unbounded_capabilities_before_operations()
    {
        for operations in [
            vec!["delete".into(), "delete".into()],
            vec!["optional".into(); rpc::OPERATION_LIMIT + 1],
            vec!["x".repeat(rpc::OPERATION_NAME_LIMIT + 1)],
            vec!["bad\nname".into()],
        ] {
            let scope = reviewed_scope();
            let expected = scope.clone();
            let (stream, mut peer) = tokio::io::duplex(8192);
            let server = tokio::spawn(async move {
                reviewed_open(&mut peer, &expected, operations).await;
                let mut eof = [0];
                assert_eq!(peer.read(&mut eof).await.unwrap(), 0);
            });
            assert!(matches!(
                Client::open_host_reviewed(
                    stream,
                    &scope.host,
                    &scope.installation,
                    CancellationToken::new()
                )
                .await,
                Err(Error::Protocol)
            ));
            tokio::time::timeout(std::time::Duration::from_secs(3), server)
                .await
                .unwrap()
                .unwrap();
        }
    }
    #[tokio::test]
    async fn reviewed_client_delete_lost_result_is_terminal_without_replay() {
        let scope = reviewed_scope();
        let expected = scope.clone();
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            reviewed_open(
                &mut peer,
                &expected,
                vec!["future_optional".into(), "delete".into()],
            )
            .await;
            let call = request(&mut peer).await;
            assert!(matches!(
                call.operation,
                Operation::ReviewEntry {
                    purpose: rpc::ReviewPurpose::Delete,
                    ..
                }
            ));
            reply(
                &mut peer,
                call.id,
                Reply::EntryReview {
                    handle: 1,
                    metadata: rpc::Metadata {
                        kind: rpc::Kind::File,
                        size: "3".into(),
                        modified_ms: None,
                    },
                },
            )
            .await;
            let call = request(&mut peer).await;
            assert!(matches!(call.operation, Operation::Delete { review: 1 }));
            // The actual request was delivered; its reply is not observed.
        });
        let (mut client, _) = Client::open_host_reviewed(
            stream,
            &scope.host,
            &scope.installation,
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
        let (review, _) = client.review_entry("/file").await.unwrap();
        assert!(matches!(
            client.delete_reviewed(&review).await,
            Err(Error::UncertainDelete(_))
        ));
        assert!(!client.available());
        assert!(client.delete_reviewed(&review).await.is_err());
        tokio::time::timeout(std::time::Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap();
    }
    fn native_reviewed_service(
        mode: u16,
        suppress_deleted_reply: bool,
    ) -> (
        std::path::PathBuf,
        DuplexStream,
        tokio::task::JoinHandle<io::Result<()>>,
        rpc::HostScope,
    ) {
        let root = crate::test_support::private_directory("festerm-reviewed-native-wire");
        std::fs::write(root.join("file"), b"owned source").unwrap();
        let filesystem = crate::filesystem::Filesystem::open(&rpc::Policy {
            root: root.clone(),
            write: true,
            delete: true,
        })
        .unwrap();
        let scope = reviewed_scope();
        let expected = scope.clone();
        let observed = root.clone();
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            write_frame(
                &mut peer,
                rpc::HOST_DESCRIPTION,
                &json(&rpc::HostDescription {
                    version: rpc::HOST_VERSION,
                    scope: expected.clone(),
                    filesystem: Some(filesystem.capabilities()),
                })
                .unwrap(),
            )
            .await?;
            let (kind, bytes) = crate::wire::read_frame(&mut peer).await?;
            assert_eq!(kind, rpc::OPEN);
            let open: rpc::HostOpen = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(open.version, mode);
            assert_eq!(open.scope, expected);
            let admission: crate::filesystem::Admission = Arc::new(move |stage| {
                if suppress_deleted_reply
                    && stage == crate::filesystem::AdmissionStage::Disclosure
                    && !observed.join("file").exists()
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                Ok(())
            });
            if mode == rpc::HOST_REVIEWED_VERSION {
                crate::filesystem::serve_authorized_reviewed(
                    peer,
                    filesystem,
                    admission,
                    CancellationToken::new(),
                )
                .await
            } else {
                crate::filesystem::serve_authorized(
                    peer,
                    filesystem,
                    admission,
                    CancellationToken::new(),
                )
                .await
            }
        });
        (root, stream, server, scope)
    }

    #[tokio::test]
    async fn reviewed_native_wire_open2_cannot_review_or_delete_even_with_protected_grant() {
        let (root, stream, server, scope) = native_reviewed_service(rpc::HOST_VERSION, false);
        let (mut client, _) = Client::open_host(
            stream,
            &scope.host,
            &scope.installation,
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(client.capabilities().version, 1);
        assert!(!client.delete_advertised());
        for operation in [
            Operation::ReviewEntry {
                path: "/file".into(),
                purpose: rpc::ReviewPurpose::Delete,
            },
            Operation::Delete { review: 1 },
        ] {
            assert!(matches!(
                client.exchange(operation, None).await,
                Err(Error::Remote {
                    code: rpc::ErrorCode::Denied,
                    ..
                })
            ));
        }
        client.ping().await.unwrap();
        assert_eq!(std::fs::read(root.join("file")).unwrap(), b"owned source");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        client.cancel();
        stopped(server).await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn reviewed_native_wire_delete_commits_once_and_suppressed_reply_stays_unknown() {
        for suppress_reply in [false, true] {
            let (root, stream, server, scope) =
                native_reviewed_service(rpc::HOST_REVIEWED_VERSION, suppress_reply);
            let (mut client, _) = Client::open_host_reviewed(
                stream,
                &scope.host,
                &scope.installation,
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(client.delete_advertised());
            let (review, metadata) = client.review_entry("/file").await.unwrap();
            assert_eq!(metadata.size, "12");
            let result = client.delete_reviewed(&review).await;
            if suppress_reply {
                assert!(matches!(result, Err(Error::UncertainDelete(_))));
                assert!(!client.available());
            } else {
                result.unwrap();
                client.ping().await.unwrap();
            }
            assert!(!root.join("file").exists());
            assert!(client.delete_reviewed(&review).await.is_err());
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
            client.cancel();
            stopped(server).await;
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
