//! Selected-host execution shared by desktop and mobile composition roots.
//! Normal bootstrap resolves an authenticated provider route and prepares a
//! purpose-bound native device key. Advanced clients retain explicit protected
//! owner approval. Public recipes grant neither role; this module never signs in
//! or enrolls an owner.

use crate::{
    host_runtime::{authenticate_native_client, StreamRole},
    host_terminal::{self, TerminalHandle},
    owner_auth::{ClientOwnerKey, OwnerBinding, OwnerKeyError},
    relay,
};
use festerm_core::Terminal;
use festerm_host_protocol::{
    Capabilities, ContractError, InventoryPage, OperationId, ProfileSummary, Request, Response,
    SessionSummary, Validate, MAX_SESSIONS, PAGE_SIZE,
};
use festerm_secret_store::SecretReference;
use festerm_tunnel_client::host_control::{Binding, ConnectionPermit, ControlClient, Error};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;
use tunnels::contracts::TunnelEndpoint;
use zeroize::Zeroizing;

pub mod approvals;
mod client_context;
pub use client_context::{ClientContext, ClientContextRef};
mod creation;
#[cfg(test)]
mod creation_tests;
mod diagnostics;
mod ending;
pub mod ordinary;
pub use diagnostics::AttachStage;
pub mod originals;
mod session;
pub(crate) use creation::create_authenticated;
pub use creation::{Created, CreationIntent, CreationMode};
pub(crate) use ending::end_authenticated;
pub use ending::EndIntent;
pub use festerm_host_protocol::{
    AttachmentId, Budgets, Creation, EnsureDisposition, LogicalSessionId, Target,
};
pub use session::HostSession;
#[cfg(feature = "testing")]
pub mod testing {
    /// Synthetic UI-only context; memory vault/private fixture storage, and all
    /// transport attempts fail closed. Never provider or native evidence.
    pub fn ordinary_context(
        index: ordinary::OrdinaryKeyIndex,
        account_id: u64,
        info: crate::ordinary_auth::OrdinaryHostInfo,
        guard: ordinary::OrdinaryAccountGuard,
    ) -> Result<ordinary::OrdinaryContext, Failure> {
        ordinary::testing_context(index, account_id, info, guard)
    }

    use super::*;

    /// In-memory admitted worker for cancellation-only composition tests.
    pub fn pending_worker(scope: Scope, cancel: CancellationToken) -> Result<Worker, Failure> {
        Worker::spawn_with_cancellation(
            scope,
            Arc::new(|| {}),
            cancel,
            |_, cancel, _, _| async move {
                cancel.cancelled().await;
                Err(Error::Cancelled.into())
            },
        )
    }

    /// Explicit authenticated control-stream injection for headless tests.
    pub async fn end_from_stream<S>(
        stream: S,
        permit: ConnectionPermit,
        binding: Binding,
        intent: Arc<EndIntent>,
        cancel: &CancellationToken,
    ) -> Result<Outcome, Failure>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let mut client = ControlClient::on_authenticated_stream(stream, binding, permit);
        end_authenticated(&mut client, intent, cancel).await
    }

    /// Explicit stream injection for headless composition tests only.
    pub fn session_from_stream<S>(
        stream: S,
        permit: ConnectionPermit,
        scope: Scope,
        target: Target,
        notifier: Arc<dyn festerm_session::SessionEventNotifier>,
    ) -> Result<Arc<HostSession>, Failure>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let notify = notifier.clone();
        let selected = target.clone();
        let worker = Worker::spawn(
            scope,
            Arc::new(move || notify.notify()),
            move |scope, cancel, events, wake| async move {
                attach_authenticated(stream, permit, &scope, target, &cancel, &events, &wake).await
            },
        )?;
        let mut session = HostSession::from_worker(worker, notifier);
        session.set_target(selected);
        Ok(Arc::new(session))
    }
}

pub const ENROLLMENT_PREREQUISITE: &str =
    "This host needs foreground host-key approval and an owner key explicitly enrolled on that host. Reopen its protected native-vault credential for this installation, account and principal; importing or saving a recipe cannot authorize a connection.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    OrdinaryKey(ordinary::KeyError),
    ProviderIdentityExpired,
    ProviderUnavailable,
    MissingApproval,
    BindingMismatch,
    OwnerKey(OwnerKeyError),
    Control(Error),
    Attachment { stage: AttachStage, error: Error },
    WorkerUnavailable,
    OriginalStorage,
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
            Self::Attachment { error, .. } => Self::Control(error).message(),
            Self::OrdinaryKey(e) => e.message(),
            Self::ProviderIdentityExpired => "GitHub sign-in expired or its account changed. Sign in again before finding hosts.",
            Self::ProviderUnavailable => "The provider could not verify the selected account and host. Retry the explicit host read.",
            Self::OriginalStorage => "The protected original-action checkpoint is missing, corrupt, incomplete, full or could not be durably updated. No replacement was issued. Explicitly reopen protected originals; storage uncertainty is not absence.",
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
            Self::Control(Error::Refused(festerm_host_protocol::Refusal::Denied)
                | Error::Contract(ContractError::Denied)) =>
                "The host denied this operation under its current remoting or creation policy. No key enrollment, takeover or replacement shell was attempted.",
            Self::Control(Error::Refused(festerm_host_protocol::Refusal::Unknown
                | festerm_host_protocol::Refusal::OutcomeUncertain)) =>
                "The host could not establish the operation's current outcome. Retain the original request; do not infer absence or create a replacement shell.",
            Self::Control(Error::Refused(festerm_host_protocol::Refusal::Busy)
                | Error::Contract(ContractError::Busy)) =>
                "The host or attachment is busy. No takeover or replacement shell was requested.",
            Self::Control(Error::Refused(festerm_host_protocol::Refusal::Unsupported)) =>
                "This host does not advertise this operation. Update and qualify the host; no legacy fallback was attempted.",
            Self::Control(Error::Cancelled) => "Host operation cancelled. No terminal input will be replayed.",
            Self::Control(Error::HostUpdateRequired) => "Update fesTerm on this host, then try again.",
            Self::Control(Error::HostPolicyRefreshRequired) => "On the HOST, explicitly Disable then Enable remote access to refresh protected policy and account readiness. Existing shells are retained. This operation was not retried.",
            Self::Control(Error::Deadline) => "The host operation timed out. Its sessions are not known to be absent.",
            Self::Control(Error::Offline | Error::Io(_) | Error::Closed) =>
                "The host connection is unavailable. Its sessions are not known to be absent.",
            Self::Control(_) => "The host refused the operation or returned an incompatible or stale result. Its outcome is not known; retain the original action rather than creating a replacement.",
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
        if cancel.is_cancelled() {
            return Err(Error::Cancelled.into());
        }
        // The worker is already admitted. Vault access is blocking and never
        // runs in a GUI callback. It cannot generate a replacement on failure.
        let approved = self.approved.clone();
        let reference = self.reference;
        let native_cancel = cancel.clone();
        let (attempt, permit) = open_native_owner_key(permit, cancel, move || {
            ClientOwnerKey::prepare_native_attempt(reference, &approved, native_cancel)
                .map_err(Failure::OwnerKey)
        })
        .await?;
        attempt
            .publication_guard()
            .check()
            .map_err(Failure::OwnerKey)?;
        let stream = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Error::Cancelled.into()),
            value = tokio::time::timeout(Duration::from_secs(15),
                relay::connect_endpoint(&self.endpoint, self.port, &self.token)) =>
                value.map_err(|_| Failure::Control(Error::Deadline).at_attach_stage_for(role, AttachStage::Relay))?
                    .map_err(|error| Failure::Control(error.into()).at_attach_stage_for(role, AttachStage::Relay))?,
        };
        let (stream, permit) =
            authenticate_native_client(stream, permit, attempt, role, cancel.clone())
                .await
                .map_err(|error| {
                    Failure::Control(error).at_attach_stage_for(role, AttachStage::Authentication)
                })?;
        Ok((stream, permit, self.binding))
    }
}

// Native consent can require multiple human-paced vault prompts. This does
// not change relay connection, authentication or control-operation deadlines.
const NATIVE_OWNER_KEY_BUDGET: Duration = Duration::from_secs(300);

async fn open_native_owner_key<T, F>(
    permit: ConnectionPermit,
    cancel: &CancellationToken,
    open: F,
) -> Result<(T, ConnectionPermit), Failure>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, Failure> + Send + 'static,
{
    if cancel.is_cancelled() {
        return Err(Error::Cancelled.into());
    }
    let permit = Arc::new(permit);
    let charge = permit.clone();
    let key_cancel = cancel.clone();
    let key = tokio::task::spawn_blocking(move || {
        let _charge = charge;
        if key_cancel.is_cancelled() {
            return Err(Failure::Control(Error::Cancelled));
        }
        open()
    });
    let key = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(Error::Cancelled.into()),
        value = tokio::time::timeout(NATIVE_OWNER_KEY_BUDGET, key) =>
            value.map_err(|_| Failure::Control(Error::Deadline))?
                .map_err(|_| Failure::WorkerUnavailable)??,
    };
    let permit = Arc::try_unwrap(permit).map_err(|_| Failure::WorkerUnavailable)?;
    Ok((key, permit))
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
    pub profiles: Vec<ProfileSummary>,
    /// An incomplete observation never becomes absence or recreation authority.
    pub unknown: Vec<Target>,
}

pub enum Operation {
    Inventory,
    EnsureProfile {
        logical: LogicalSessionId,
        revision: festerm_host_protocol::Epoch,
        attachment: AttachmentId,
    },
    Attach {
        target: Target,
    },
    Create(Arc<CreationIntent>),
    End(Arc<EndIntent>),
    AttachReserved {
        target: Target,
        attachment: AttachmentId,
    },
}

/// An existing target selected after foreground approval. This command carries
/// protected context, never a portable recipe or a service-token grant.
pub struct ApprovedAttachment {
    context: ClientContext,
    scope: Scope,
    target: Target,
    attachment: Option<AttachmentId>,
    disposition: Option<festerm_host_protocol::EnsureDisposition>,
    creation_intent: Option<Arc<CreationIntent>>,
}

impl std::fmt::Debug for ApprovedAttachment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApprovedAttachment")
            .finish_non_exhaustive()
    }
}

impl ApprovedAttachment {
    pub fn new(
        context: impl Into<ClientContext>,
        scope: Scope,
        target: Target,
    ) -> Result<Self, Failure> {
        let context = context.into();
        if context.binding() != &scope.binding {
            return Err(Failure::BindingMismatch);
        }
        Ok(Self {
            context,
            scope,
            target,
            attachment: None,
            disposition: None,
            creation_intent: None,
        })
    }

    pub fn from_created(
        context: impl Into<ClientContext>,
        scope: Scope,
        created: Created,
    ) -> Result<Self, Failure> {
        let context = context.into();
        if !created.intent.matches(&context) || context.binding() != &scope.binding {
            return Err(Failure::BindingMismatch);
        }
        let disposition = Some(created.disposition());
        Ok(Self {
            context,
            scope,
            target: created.session.target,
            attachment: Some(created.attachment),
            disposition,
            creation_intent: Some(created.intent),
        })
    }

    pub fn from_profile_ensured(
        context: impl Into<ClientContext>,
        scope: Scope,
        session: SessionSummary,
        disposition: EnsureDisposition,
        attachment: AttachmentId,
    ) -> Result<Self, Failure> {
        let context = context.into();
        if context.binding() != &scope.binding {
            return Err(Failure::BindingMismatch);
        }
        Ok(Self {
            context,
            scope,
            target: session.target,
            attachment: Some(attachment),
            disposition: Some(disposition),
            creation_intent: None,
        })
    }

    pub fn description(&self) -> String {
        use festerm_host_protocol::EnsureDisposition;
        let state = match self.disposition {
            Some(EnsureDisposition::Created) => {
                "Approved HOST · host confirmed creation; attaching generation"
            }
            Some(EnsureDisposition::Recreated) => {
                "Approved HOST · host confirmed recreation; attaching generation"
            }
            Some(EnsureDisposition::Resumed) => {
                "Approved HOST · host confirmed resume; attaching generation"
            }
            None => "Approved HOST · attaching selected existing generation",
        };
        format!(
            "{state} · logical {} · generation {}",
            self.target.logical,
            self.target.generation.get()
        )
    }

    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    pub fn port(&self) -> u16 {
        self.context.port()
    }

    pub fn start(
        self,
        budgets: &Budgets,
        notifier: Arc<dyn festerm_session::SessionEventNotifier>,
    ) -> Result<HostSession, Failure> {
        self.start_with_cancellation(budgets, notifier, CancellationToken::new())
    }

    pub fn start_with_cancellation(
        self,
        budgets: &Budgets,
        notifier: Arc<dyn festerm_session::SessionEventNotifier>,
        cancel: CancellationToken,
    ) -> Result<HostSession, Failure> {
        let operation = match self.attachment {
            None => Operation::Attach {
                target: self.target,
            },
            Some(attachment) => Operation::AttachReserved {
                target: self.target,
                attachment,
            },
        };
        let mut session = HostSession::start_operation_with_cancellation(
            self.context,
            self.scope,
            operation,
            budgets,
            notifier,
            cancel,
        )?;
        session.set_creation_intent(self.creation_intent);
        Ok(session)
    }
}

pub enum Outcome {
    Inventory(Inventory),
    Created(Box<Created>),
    ProfileEnsured {
        session: SessionSummary,
        disposition: EnsureDisposition,
        attachment: AttachmentId,
    },
    Ended {
        target: Target,
    },
    Snapshot(Box<Adoption>),
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
    _snapshot_permit: Arc<festerm_host_protocol::SnapshotPermit>,
}

impl Adoption {
    pub fn adopt_into(self, current: &Scope, terminal: &mut Terminal) -> Result<(), Failure> {
        if &self.scope != current || self.acknowledge.is_closed() {
            return Err(ContractError::Stale.into());
        }
        let previous = std::mem::replace(terminal, self.terminal);
        if self.acknowledge.send(()).is_err() {
            *terminal = previous;
            return Err(Error::Cancelled.into());
        }
        Ok(())
    }
}

/// One admitted selected-host operation, not a discovery/fan-out scheduler.
/// Keep this owner alive alongside a live terminal. Dropping it cancels the
/// runtime, never reports Detached and never reconnects or replays input.
pub struct Worker {
    scope: Scope,
    cancel: CancellationToken,
    events: flume::Receiver<Completion>,
    ordinary_guard: Option<ordinary::OrdinaryAccountGuard>,
}

impl Worker {
    pub fn start(
        context: impl Into<ClientContext>,
        scope: Scope,
        operation: Operation,
        budgets: &Budgets,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, Failure> {
        Self::start_with_cancellation(
            context,
            scope,
            operation,
            budgets,
            wake,
            CancellationToken::new(),
        )
    }

    pub fn start_with_cancellation(
        context: impl Into<ClientContext>,
        scope: Scope,
        operation: Operation,
        budgets: &Budgets,
        wake: Arc<dyn Fn() + Send + Sync>,
        cancel: CancellationToken,
    ) -> Result<Self, Failure> {
        let context = context.into();
        if cancel.is_cancelled() {
            return Err(Error::Cancelled.into());
        }
        if context.binding() != &scope.binding {
            return Err(Failure::BindingMismatch);
        }
        if let Operation::Create(intent) = &operation {
            if !intent.matches(&context) {
                return Err(Failure::BindingMismatch);
            }
        }
        if let Operation::End(intent) = &operation {
            if !intent.matches_context(context.as_enrolled_owner().ok_or(ContractError::Denied)?) {
                return Err(Failure::BindingMismatch);
            }
        }
        if let Some(guard) = context.ordinary_guard() {
            guard.check()?;
        }
        // Admission precedes thread/runtime allocation, vault lookup and connect.
        let permit = ConnectionPermit::reserve(budgets)?;
        Self::spawn_with_guard(
            scope,
            wake,
            cancel,
            context.ordinary_guard(),
            move |scope, cancel, events, wake| async move {
                execute(context, permit, &scope, operation, &cancel, &events, &wake).await
            },
        )
    }

    #[cfg(any(all(test, feature = "relay", unix), feature = "testing"))]
    pub(crate) fn spawn<F, Fut>(
        scope: Scope,
        wake: Arc<dyn Fn() + Send + Sync>,
        task: F,
    ) -> Result<Self, Failure>
    where
        F: FnOnce(
                Scope,
                CancellationToken,
                flume::Sender<Completion>,
                Arc<dyn Fn() + Send + Sync>,
            ) -> Fut
            + Send
            + 'static,
        Fut: std::future::Future<Output = Result<Outcome, Failure>>,
    {
        Self::spawn_with_cancellation(scope, wake, CancellationToken::new(), task)
    }

    #[cfg(any(all(test, feature = "relay", unix), feature = "testing"))]
    fn spawn_with_cancellation<F, Fut>(
        scope: Scope,
        wake: Arc<dyn Fn() + Send + Sync>,
        cancel: CancellationToken,
        task: F,
    ) -> Result<Self, Failure>
    where
        F: FnOnce(
                Scope,
                CancellationToken,
                flume::Sender<Completion>,
                Arc<dyn Fn() + Send + Sync>,
            ) -> Fut
            + Send
            + 'static,
        Fut: std::future::Future<Output = Result<Outcome, Failure>>,
    {
        Self::spawn_with_guard(scope, wake, cancel, None, task)
    }

    fn spawn_with_guard<F, Fut>(
        scope: Scope,
        wake: Arc<dyn Fn() + Send + Sync>,
        cancel: CancellationToken,
        ordinary_guard: Option<ordinary::OrdinaryAccountGuard>,
        task: F,
    ) -> Result<Self, Failure>
    where
        F: FnOnce(
                Scope,
                CancellationToken,
                flume::Sender<Completion>,
                Arc<dyn Fn() + Send + Sync>,
            ) -> Fut
            + Send
            + 'static,
        Fut: std::future::Future<Output = Result<Outcome, Failure>>,
    {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled.into());
        }
        let (events, receiver) = flume::bounded(2);
        let worker_cancel = cancel.clone();
        let worker_scope = scope.clone();
        let lifetime = ordinary_guard.as_ref().map(|guard| guard.cancel.clone());
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
                    let cancelled_wake=wake.clone();
                    let served=async {
                    let result = task(
                        worker_scope.clone(),
                        worker_cancel.clone(),
                        events.clone(),
                        wake.clone(),
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
                    };
                    if let Some(lifetime)=lifetime {
                        tokio::select! { biased; _=lifetime.cancelled()=>{worker_cancel.cancel();cancelled_wake();}, _=served=>{} }
                    } else { served.await; }
                });
            })
            .map_err(|_| Failure::WorkerUnavailable)?;
        Ok(Self {
            scope,
            cancel,
            events: receiver,
            ordinary_guard,
        })
    }

    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
            || self
                .ordinary_guard
                .as_ref()
                .is_some_and(|guard| guard.check().is_err())
    }

    pub fn try_receive(&self, current: &Scope) -> Option<Completion> {
        if current != &self.scope || self.is_cancelled() {
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
    context: ClientContext,
    permit: ConnectionPermit,
    scope: &Scope,
    operation: Operation,
    cancel: &CancellationToken,
    events: &flume::Sender<Completion>,
    wake: &Arc<dyn Fn() + Send + Sync>,
) -> Result<Outcome, Failure> {
    let role = match operation {
        Operation::Inventory
        | Operation::EnsureProfile { .. }
        | Operation::Create(_)
        | Operation::End(_) => StreamRole::Control,
        Operation::Attach { .. } | Operation::AttachReserved { .. } => StreamRole::Terminal,
    };
    let (stream, permit, binding) = context.connect(permit, role, cancel).await?;
    match operation {
        Operation::Inventory => {
            let mut client = ControlClient::on_authenticated_stream(stream, binding, permit);
            Ok(Outcome::Inventory(
                inventory_authenticated(&mut client, scope.request.clone(), cancel).await?,
            ))
        }
        Operation::EnsureProfile {
            logical,
            revision,
            attachment,
        } => {
            let mut client = ControlClient::on_authenticated_stream(stream, binding, permit);
            let (session, disposition, attachment) =
                ensure_profile_authenticated(&mut client, logical, revision, attachment, cancel)
                    .await?;
            Ok(Outcome::ProfileEnsured {
                session,
                disposition,
                attachment,
            })
        }
        Operation::Attach { target } => {
            debug_assert_eq!(&binding, &scope.binding);
            attach_authenticated(stream, permit, scope, target, cancel, events, wake).await
        }
        Operation::Create(intent) => {
            let mut client = ControlClient::on_authenticated_stream(stream, binding, permit);
            create_authenticated(&mut client, intent, cancel).await
        }
        Operation::End(intent) => {
            let mut client = ControlClient::on_authenticated_stream(stream, binding, permit);
            end_authenticated(&mut client, intent, cancel).await
        }
        Operation::AttachReserved { target, attachment } => {
            attach_reserved_authenticated(
                stream,
                permit,
                scope,
                (target, attachment),
                cancel,
                events,
                wake,
            )
            .await
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
    attach_reserved_authenticated(
        stream,
        permit,
        scope,
        (target, AttachmentId::new()),
        cancel,
        events,
        wake,
    )
    .await
}

pub(crate) async fn attach_reserved_authenticated<S>(
    stream: S,
    permit: ConnectionPermit,
    scope: &Scope,
    selection: (Target, AttachmentId),
    cancel: &CancellationToken,
    events: &flume::Sender<Completion>,
    wake: &Arc<dyn Fn() + Send + Sync>,
) -> Result<Outcome, Failure>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut pending = host_terminal::establish_diagnostic(
        stream,
        permit,
        scope.binding.clone(),
        scope.request.clone(),
        selection.0,
        selection.1,
        false,
        cancel,
    )
    .await
    .map_err(|(stage, error)| Failure::Attachment { stage, error })?;
    let terminal = pending.take_snapshot().ok_or(ContractError::Denied)?;
    let (acknowledge, adopted) = tokio::sync::oneshot::channel();
    let completion = Completion {
        scope: scope.clone(),
        outcome: Outcome::Snapshot(Box::new(Adoption {
            _snapshot_permit: pending.snapshot_permit(),
            scope: scope.clone(),
            terminal,
            acknowledge,
        })),
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
            result.map_err(|_| Failure::Attachment { stage: AttachStage::FrontendAdoption, error: Error::Deadline })?
                .map_err(|_| Failure::Attachment { stage: AttachStage::FrontendAdoption, error: Error::Cancelled })?,
    }
    // Only host-committed activation may publish Attached.
    Ok(Outcome::Attached(
        pending
            .adopt(cancel)
            .await
            .map_err(|error| Failure::Control(error).at_attach_stage(AttachStage::HostActivation))?
            .spawn()?,
    ))
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
        profiles: Vec::new(),
        unknown: Vec::new(),
    };
    let mut complete = false;
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
            complete = true;
            break;
        }
        previous = Some(page);
    }
    if !complete {
        return Err(ContractError::Limit.into());
    }
    if inventory.capabilities.profiles {
        let response = client
            .request(OperationId::new(), Request::ListProfiles {}, cancel)
            .await?;
        match response.body {
            Response::Profiles { profiles } => inventory.profiles = profiles,
            Response::Refused { reason } => return Err(Error::Refused(reason)),
            _ => return Err(ContractError::Malformed.into()),
        }
    }
    Ok(inventory)
}

async fn ensure_profile_authenticated<S: AsyncRead + AsyncWrite + Unpin>(
    client: &mut ControlClient<S>,
    logical: LogicalSessionId,
    revision: festerm_host_protocol::Epoch,
    attachment: AttachmentId,
    cancel: &CancellationToken,
) -> Result<(SessionSummary, EnsureDisposition, AttachmentId), Failure> {
    let capabilities = client.handshake(cancel).await?;
    if !capabilities.ensure_profile || !capabilities.attach {
        return Err(Error::Refused(festerm_host_protocol::Refusal::Unsupported).into());
    }
    let issued = client.issue_operation()?;
    let response = client
        .request_issued(
            &issued,
            Request::EnsureProfile {
                logical,
                revision,
                attachment: attachment.clone(),
            },
            cancel,
        )
        .await?;
    match response.body {
        Response::Ensured {
            session,
            disposition,
            lease: None,
        } => Ok((session, disposition, attachment)),
        Response::Ensured { .. } => Err(ContractError::Malformed.into()),
        Response::Refused { reason } => Err(Error::Refused(reason).into()),
        _ => Err(ContractError::Malformed.into()),
    }
}

#[cfg(test)]
mod tests;
