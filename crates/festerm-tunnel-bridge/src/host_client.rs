//! Selected-host execution shared by desktop and mobile composition roots.
//! Public recipes are not credentials. The caller supplies an already approved
//! protected owner context; this module never enrolls, signs in or discovers.

use crate::{
    host_runtime::{authenticate_client, StreamRole},
    host_terminal::{self, TerminalHandle},
    owner_auth::{ClientOwnerKey, OwnerBinding, OwnerKeyError},
    relay,
};
use festerm_core::Terminal;
use festerm_host_protocol::{
    AttachmentId, Budgets, Capabilities, ContractError, InventoryPage, OperationId, Request,
    Response, SessionSummary, Target, Validate, MAX_SESSIONS, PAGE_SIZE,
};
use festerm_secret_store::SecretReference;
use festerm_tunnel_client::host_control::{Binding, ConnectionPermit, ControlClient, Error};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;
use tunnels::contracts::TunnelEndpoint;
use zeroize::Zeroizing;

pub const ENROLLMENT_PREREQUISITE: &str =
    "This host needs foreground host-key approval and an owner key explicitly enrolled on that host. Reopen its protected native-vault credential for this installation, account and principal; importing or saving a recipe cannot authorize a connection.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    MissingApproval,
    BindingMismatch,
    OwnerKey(OwnerKeyError),
    Control(Error),
    WorkerUnavailable,
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self::Control(error)
    }
}

impl From<ContractError> for Failure {
    fn from(error: ContractError) -> Self {
        Self::Control(error.into())
    }
}

impl Failure {
    pub fn message(self) -> &'static str {
        match self {
            Self::MissingApproval => ENROLLMENT_PREREQUISITE,
            Self::BindingMismatch => "The approved host, key, account or endpoint changed. Review the selected host in the foreground; the saved recipe was not changed.",
            Self::OwnerKey(OwnerKeyError::Vault(festerm_secret_store::SecretStoreError::Missing)) =>
                "The approved owner key is missing. Complete explicit host-local enrollment; no replacement key was created.",
            Self::OwnerKey(OwnerKeyError::Vault(festerm_secret_store::SecretStoreError::LockedOrUnavailable)) =>
                "The native owner vault is locked or unavailable. Unlock it and retry; no plaintext fallback was attempted.",
            Self::OwnerKey(OwnerKeyError::Vault(festerm_secret_store::SecretStoreError::Unsupported)) =>
                "Native owner credential storage is unsupported on this platform. No plaintext fallback is allowed.",
            Self::OwnerKey(_) => "The approved owner key could not be reopened for this exact installation, host, account and principal. Review its protected approval; no replacement key was created.",
            Self::Control(Error::Refused(festerm_host_protocol::Refusal::Revoked)) =>
                "Host access was revoked. Fresh explicit host-local authorization is required.",
            Self::Control(Error::Refused(festerm_host_protocol::Refusal::Busy)
                | Error::Contract(ContractError::Busy)) =>
                "The host or attachment is busy. No takeover or replacement shell was requested.",
            Self::Control(Error::Refused(festerm_host_protocol::Refusal::Unsupported)) =>
                "This host does not advertise this operation. Update and qualify the host; no legacy fallback was attempted.",
            Self::Control(Error::Cancelled) => "Host operation cancelled. No terminal input will be replayed.",
            Self::Control(Error::Deadline) => "The host operation timed out. Its sessions are not known to be absent.",
            Self::Control(Error::Offline | Error::Closed) =>
                "The host connection is unavailable. Its sessions are not known to be absent.",
            Self::Control(_) => "The host refused the operation or returned an incompatible or stale result. No shell was created.",
            Self::WorkerUnavailable => "The bounded host worker could not start. Retry the explicit action.",
        }
    }
}

/// Never serializable or reconstructible from portable recipe metadata.
pub struct ApprovedContext {
    approved: OwnerBinding,
    reference: SecretReference,
    endpoint: TunnelEndpoint,
    port: u16,
    token: Zeroizing<String>,
    binding: Binding,
}

impl ApprovedContext {
    /// The composition root must obtain this context from protected foreground
    /// approval, not a discovery row. Opening the key revalidates the full
    /// stored binding again in the bounded worker before any network access.
    pub fn from_protected_approval(
        approved: OwnerBinding,
        reference: SecretReference,
        endpoint: TunnelEndpoint,
        port: u16,
        token: Zeroizing<String>,
        binding: Binding,
    ) -> Result<Self, Failure> {
        approved.validate()?;
        if binding.host() != &approved.host.host
            || binding.account() != approved.account
            || port == 0
            || token.is_empty()
            || token.len() > 16 * 1024
            || endpoint.host_public_keys.len() != 1
            || endpoint.host_public_keys[0].len() > 4096
            || relay::host_fingerprint(&endpoint.host_public_keys[0])
                .map_err(|_| Failure::BindingMismatch)?
                != approved.host.host_key_fingerprint
        {
            return Err(Failure::BindingMismatch);
        }
        Ok(Self {
            approved,
            reference,
            endpoint,
            port,
            token,
            binding,
        })
    }

    pub fn binding(&self) -> &Binding {
        &self.binding
    }

    async fn connect(
        self,
        permit: ConnectionPermit,
        role: StreamRole,
        cancel: &CancellationToken,
    ) -> Result<(relay::ClientConnection, ConnectionPermit, Binding), Failure> {
        // The worker is already admitted. Vault access is blocking and never
        // runs in a GUI callback. It cannot generate a replacement on failure.
        let permit = Arc::new(permit);
        let charge = permit.clone();
        let approved = self.approved.clone();
        let reference = self.reference;
        let key = tokio::task::spawn_blocking(move || {
            let _charge = charge;
            ClientOwnerKey::open_native(reference, &approved)
        });
        let key = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Error::Cancelled.into()),
            value = key => value.map_err(|_| Failure::WorkerUnavailable)?
                .map_err(Failure::OwnerKey)?,
        };
        let permit = Arc::try_unwrap(permit).map_err(|_| Failure::WorkerUnavailable)?;
        let stream = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Error::Cancelled.into()),
            value = tokio::time::timeout(Duration::from_secs(15),
                relay::connect_endpoint(&self.endpoint, self.port, &self.token)) =>
                value.map_err(|_| Failure::Control(Error::Deadline))?
                    .map_err(|_| Failure::Control(Error::Offline))?,
        };
        let (stream, permit) =
            authenticate_client(stream, permit, key, self.approved, role, cancel.clone()).await?;
        Ok((stream, permit, self.binding))
    }
}

/// Original editor/tab request plus its account/endpoint and lifecycle epoch.
/// A mobile app-lock/account change invalidates the epoch and cancels its worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Scope {
    pub request: OperationId,
    pub generation: u64,
    pub binding: Binding,
}

#[derive(Clone, Debug)]
pub struct Inventory {
    pub capabilities: Capabilities,
    pub sessions: Vec<SessionSummary>,
    /// An incomplete observation never becomes absence or recreation authority.
    pub unknown: Vec<Target>,
}

pub enum Operation {
    Inventory,
    Attach { target: Target },
}

pub enum Outcome {
    Inventory(Inventory),
    Snapshot(Adoption),
    Attached(TerminalHandle),
    Failed(Failure),
}

pub struct Completion {
    pub scope: Scope,
    pub outcome: Outcome,
}

/// A snapshot is not Attached. Only the exact originating frontend may install
/// it and acknowledge adoption; dropping it refuses the pending attachment.
pub struct Adoption {
    scope: Scope,
    terminal: Terminal,
    acknowledge: tokio::sync::oneshot::Sender<()>,
}

impl Adoption {
    pub fn adopt_into(self, current: &Scope, terminal: &mut Terminal) -> Result<(), Failure> {
        if &self.scope != current || self.acknowledge.is_closed() {
            return Err(ContractError::Stale.into());
        }
        *terminal = self.terminal;
        self.acknowledge
            .send(())
            .map_err(|_| Failure::Control(Error::Cancelled))
    }
}

/// One admitted selected-host operation, not a discovery/fan-out scheduler.
/// Keep this owner alive alongside a live terminal. Dropping it cancels the
/// runtime, never reports Detached and never reconnects or replays input.
pub struct Worker {
    scope: Scope,
    cancel: CancellationToken,
    events: flume::Receiver<Completion>,
}

impl Worker {
    pub fn start(
        context: ApprovedContext,
        scope: Scope,
        operation: Operation,
        budgets: &Budgets,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, Failure> {
        if context.binding() != &scope.binding {
            return Err(Failure::BindingMismatch);
        }
        // Admission precedes thread/runtime allocation, vault lookup and connect.
        let permit = ConnectionPermit::reserve(budgets)?;
        let cancel = CancellationToken::new();
        let (events, receiver) = flume::bounded(2);
        let worker_cancel = cancel.clone();
        let worker_scope = scope.clone();
        std::thread::Builder::new()
            .name("festerm-selected-host".into())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    if events
                        .try_send(Completion {
                            scope: worker_scope,
                            outcome: Outcome::Failed(Failure::WorkerUnavailable),
                        })
                        .is_ok()
                    {
                        wake();
                    }
                    return;
                };
                runtime.block_on(async {
                    let result = execute(
                        context,
                        permit,
                        &worker_scope,
                        operation,
                        &worker_cancel,
                        &events,
                        &wake,
                    )
                    .await;
                    let live = matches!(result, Ok(Outcome::Attached(_)));
                    let completion = Completion {
                        scope: worker_scope,
                        outcome: result.unwrap_or_else(Outcome::Failed),
                    };
                    tokio::select! {
                        biased;
                        _ = worker_cancel.cancelled() => return,
                        result = events.send_async(completion) => if result.is_err() { return },
                    }
                    wake();
                    if live {
                        // TerminalHandle's actor belongs to this runtime.
                        worker_cancel.cancelled().await;
                    }
                });
            })
            .map_err(|_| Failure::WorkerUnavailable)?;
        Ok(Self {
            scope,
            cancel,
            events: receiver,
        })
    }

    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn try_receive(&self, current: &Scope) -> Option<Completion> {
        if current != &self.scope || self.cancel.is_cancelled() {
            self.cancel();
            return None;
        }
        self.events
            .try_recv()
            .ok()
            .filter(|event| &event.scope == current)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn execute(
    context: ApprovedContext,
    permit: ConnectionPermit,
    scope: &Scope,
    operation: Operation,
    cancel: &CancellationToken,
    events: &flume::Sender<Completion>,
    wake: &Arc<dyn Fn() + Send + Sync>,
) -> Result<Outcome, Failure> {
    let role = match operation {
        Operation::Inventory => StreamRole::Control,
        Operation::Attach { .. } => StreamRole::Terminal,
    };
    let (stream, permit, binding) = context.connect(permit, role, cancel).await?;
    match operation {
        Operation::Inventory => {
            let mut client = ControlClient::on_authenticated_stream(stream, binding, permit);
            Ok(Outcome::Inventory(
                inventory_authenticated(&mut client, scope.request.clone(), cancel).await?,
            ))
        }
        Operation::Attach { target } => {
            debug_assert_eq!(&binding, &scope.binding);
            attach_authenticated(stream, permit, scope, target, cancel, events, wake).await
        }
    }
}

pub(crate) async fn attach_authenticated<S>(
    stream: S,
    permit: ConnectionPermit,
    scope: &Scope,
    target: Target,
    cancel: &CancellationToken,
    events: &flume::Sender<Completion>,
    wake: &Arc<dyn Fn() + Send + Sync>,
) -> Result<Outcome, Failure>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut pending = host_terminal::establish(
        stream,
        permit,
        scope.binding.clone(),
        scope.request.clone(),
        target,
        AttachmentId::new(),
        false,
        cancel,
    )
    .await?;
    let terminal = pending.take_snapshot().ok_or(ContractError::Denied)?;
    let (acknowledge, adopted) = tokio::sync::oneshot::channel();
    let completion = Completion {
        scope: scope.clone(),
        outcome: Outcome::Snapshot(Adoption {
            scope: scope.clone(),
            terminal,
            acknowledge,
        }),
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(Error::Cancelled.into()),
        result = events.send_async(completion) =>
            result.map_err(|_| Failure::Control(Error::Cancelled))?,
    }
    wake();
    tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(Error::Cancelled.into()),
        result = tokio::time::timeout(Duration::from_secs(15), adopted) =>
            result.map_err(|_| Failure::Control(Error::Deadline))?
                .map_err(|_| Failure::Control(Error::Cancelled))?,
    }
    // Only host-committed activation may publish Attached.
    Ok(Outcome::Attached(pending.adopt(cancel).await?.spawn()?))
}

pub(crate) async fn inventory_authenticated<S: AsyncRead + AsyncWrite + Unpin>(
    client: &mut ControlClient<S>,
    operation: OperationId,
    cancel: &CancellationToken,
) -> Result<Inventory, Error> {
    let capabilities = client.handshake(cancel).await?;
    if !capabilities.inventory {
        return Err(Error::Refused(festerm_host_protocol::Refusal::Unsupported));
    }
    let mut previous: Option<InventoryPage> = None;
    let mut inventory = Inventory {
        capabilities,
        sessions: Vec::new(),
        unknown: Vec::new(),
    };
    for _ in 0..MAX_SESSIONS / PAGE_SIZE {
        let cursor = previous.as_ref().and_then(|page| page.next.clone());
        let response = client
            .request(operation.clone(), Request::List { cursor }, cancel)
            .await?;
        let page = match response.body {
            Response::Inventory { page } => page,
            Response::Refused { reason } => return Err(Error::Refused(reason)),
            _ => return Err(ContractError::Malformed.into()),
        };
        if let Some(previous) = &previous {
            if previous.snapshot != page.snapshot
                || previous.authorization_epoch != page.authorization_epoch
                || previous.unknown != page.unknown
                || inventory
                    .sessions
                    .last()
                    .zip(page.sessions.first())
                    .is_some_and(|(last, first)| last.target.logical >= first.target.logical)
            {
                return Err(ContractError::Stale.into());
            }
        }
        inventory.sessions.extend(page.sessions.iter().cloned());
        if inventory.sessions.len() + page.unknown.len() > MAX_SESSIONS {
            return Err(ContractError::Limit.into());
        }
        if page.next.is_none() {
            inventory.unknown = page.unknown;
            return Ok(inventory);
        }
        previous = Some(page);
    }
    Err(ContractError::Limit.into())
}

#[cfg(test)]
mod tests;
