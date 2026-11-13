//! HOST dispatcher core, composed only by the explicit authenticated runtime.
//!
//! Installed endpoint activation and protected enrollment remain integration
//! gates; a bearer connection is not an owner.

use crate::owner_auth::{OwnerConnectionGuard, OwnerVerifier, VerifiedOwner};
use festerm_host_protocol::{
    AdmissionFence, BudgetPermit, Capabilities, ContractError, Envelope, Epoch, Refusal, Request,
    Resource, Response, SessionGrant, CONTROL_DEADLINE_SECONDS, CONTROL_FRAME_BYTES,
};
pub use festerm_tunnel_client::host_control::ConnectionPermit;
use festerm_tunnel_client::host_control::{read_frame_started, Error};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

#[cfg(feature = "relay")]
pub mod native;

#[cfg(test)]
#[path = "host_control_tests.rs"]
pub(crate) mod tests;

/// Implement only using current protected enrollment. Keep its admission guard
/// through `check`; never reconstruct policy from portable/client metadata.
pub trait AuthorizationSource: Send + Sync + 'static {
    fn with_current(
        &self,
        check: &mut dyn FnMut(&OwnerVerifier) -> Result<(), ContractError>,
    ) -> Result<(), ContractError>;
}

/// An owned policy lease keeps protected consent stable through a native
/// reservation/publication, but is dropped before native startup/IPC waits.
pub trait PolicyGuard {
    fn verifier(&self) -> &OwnerVerifier;
}

pub trait ProtectedPolicySource: Send + Sync + 'static {
    type Guard: PolicyGuard;
    fn current_policy(&self) -> Result<Self::Guard, ContractError>;
}

impl<A: ProtectedPolicySource> AuthorizationSource for A {
    fn with_current(
        &self,
        check: &mut dyn FnMut(&OwnerVerifier) -> Result<(), ContractError>,
    ) -> Result<(), ContractError> {
        let guard = self.current_policy()?;
        check(guard.verifier())
    }
}

/// Small trusted session-manager seam, not a second registry or spawn policy.
/// `execute` must authorize against fresh protected state under its reservation
/// before each effect and publication. Unknown/revoked is never absence.
/// `Capabilities`/inventory also require `admission.capture(None, epoch)`.
/// Native waits must not hold enrollment/journal locks; late work reconciles
/// its actual result. No implementation/listener is installed by this core.
pub trait SessionBackend: Send + Sync + 'static {
    fn execute(
        &self,
        request: &Envelope<Request>,
        admission: &OperationAdmission,
    ) -> Result<Response, Refusal>;
}

pub struct OperationAdmission {
    owner: Arc<VerifiedOwner>,
    source: Arc<dyn AuthorizationSource>,
    request: Envelope<Request>,
    cancelled: Arc<AtomicBool>,
    #[cfg(feature = "relay")]
    inventory: Arc<std::sync::Mutex<Option<InventoryCapture>>>,
}

#[cfg(feature = "relay")]
struct InventoryCapture {
    snapshot: festerm_host_protocol::AuthorizedInventory,
    grants: Vec<SessionGrant>,
    unknown: Vec<festerm_host_protocol::Target>,
    expires: std::time::Instant,
}

#[cfg(feature = "relay")]
struct InventoryLifetime(Arc<std::sync::Mutex<Option<InventoryCapture>>>);

#[cfg(feature = "relay")]
impl Drop for InventoryLifetime {
    fn drop(&mut self) {
        if let Ok(mut snapshot) = self.0.lock() {
            *snapshot = None;
        }
    }
}

impl OperationAdmission {
    #[cfg(feature = "relay")]
    pub(crate) fn terminal(
        owner: Arc<VerifiedOwner>,
        source: Arc<dyn AuthorizationSource>,
        request: Envelope<Request>,
        cancelled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            owner,
            source,
            request,
            cancelled,
            inventory: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn principal(&self) -> &festerm_host_protocol::PrincipalId {
        self.owner.principal()
    }

    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn check_host(&self) -> Result<(), ContractError> {
        let mut request = self.request.clone();
        request.body = Request::Capabilities {};
        request.issuance_epoch = None;
        self.source.with_current(&mut |current| {
            if self.cancelled() {
                return Err(ContractError::Stale);
            }
            self.owner
                .authorize(current, &request, Epoch::new(1)?, None)?;
            Ok(())
        })
    }

    /// Call inside the backend reservation, using its current record and epoch,
    /// never a caller-asserted or previously captured grant.
    pub fn capture(
        &self,
        grant: Option<&SessionGrant>,
        operation_epoch: Epoch,
    ) -> Result<AdmissionFence, ContractError> {
        if self.cancelled() {
            return Err(ContractError::Stale);
        }

        let mut fence = None;
        self.source.with_current(&mut |current| {
            if self.cancelled() {
                return Err(ContractError::Stale);
            }
            fence = Some(
                self.owner
                    .authorize(current, &self.request, operation_epoch, grant)?,
            );
            Ok(())
        })?;
        fence.ok_or(ContractError::Denied)
    }

    #[cfg(feature = "relay")]
    pub(crate) fn authorize_current(
        &self,
        current: &OwnerVerifier,
        request: &Envelope<Request>,
        grant: Option<&SessionGrant>,
        operation_epoch: Epoch,
    ) -> Result<AdmissionFence, ContractError> {
        if self.cancelled() {
            return Err(ContractError::Stale);
        }
        self.owner
            .authorize(current, request, operation_epoch, grant)
    }

    pub fn revalidate(
        &self,
        captured: &AdmissionFence,
        grant: Option<&SessionGrant>,
        operation_epoch: Epoch,
    ) -> Result<(), ContractError> {
        captured.revalidate(&self.capture(grant, operation_epoch)?)
    }
}

struct CancelOperation(Arc<AtomicBool>);
impl Drop for CancelOperation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

pub const CONTROL_IDLE_SECONDS: u64 = 300;
const REFUSAL_BYTES: usize = 512;

struct Publication {
    body: Response,
    // Retained by blocking work, then by publication, including after a lost
    // reply. Never release/reacquire the reply allocation around a commit.
    _operation: Option<BudgetPermit>,
    bytes: Option<BudgetPermit>,
    _create: Option<BudgetPermit>,
}

impl Publication {
    fn refused(reason: Refusal) -> Self {
        Self {
            body: Response::Refused { reason },
            _operation: None,
            bytes: None,
            _create: None,
        }
    }
}

fn execute(
    backend: &dyn SessionBackend,
    admission: &OperationAdmission,
    permit: &ConnectionPermit,
    capabilities: Option<Capabilities>,
) -> Result<Publication, Error> {
    match admission.check_host() {
        Ok(()) => {}
        Err(ContractError::Denied | ContractError::Stale) => {
            return Err(ContractError::Denied.into())
        }
        Err(ContractError::Busy) => return Ok(Publication::refused(Refusal::Busy)),
        Err(_) => return Ok(Publication::refused(Refusal::Unknown)),
    }
    if capabilities.is_some_and(|caps| !caps.supports(&admission.request.body)) {
        return Ok(Publication::refused(Refusal::Unsupported));
    }
    let mut publication = Publication::refused(Refusal::Busy);
    let reservations = (|| {
        publication._operation = Some(permit.budgets().reserve(Resource::Control, 1)?);
        publication.bytes = Some(
            permit
                .budgets()
                .reserve(Resource::ControlBytes, CONTROL_FRAME_BYTES)?,
        );
        if matches!(
            admission.request.body,
            Request::Ensure { .. } | Request::Create { .. } | Request::EnsureProfile { .. }
        ) {
            publication._create = Some(permit.budgets().reserve(Resource::PendingCreate, 1)?);
        }
        Ok::<_, ContractError>(())
    })();
    if let Err(error) = reservations {
        if error == ContractError::Busy {
            return Ok(publication);
        }
        return Err(error.into());
    }
    publication.body = backend
        .execute(&admission.request, admission)
        .unwrap_or_else(|reason| Response::Refused { reason });
    match admission.check_host() {
        Ok(()) => {}
        Err(ContractError::Denied | ContractError::Stale) => {
            return Err(ContractError::Denied.into())
        }
        // The backend may already have committed. Contention is not denial,
        // cancellation, or authoritative absence.
        Err(_) => {
            publication.body = Response::Refused {
                reason: Refusal::OutcomeUncertain,
            }
        }
    }
    Ok(publication)
}

/// Test/integration core only: `owner` and `guard` must come from consumed proof
/// on this exact pinned stream. There is deliberately no constructor accepting
/// a principal, bearer token, or `trusted: bool`.
///
/// One operation per connection at a time; terminal traffic uses a distinct
/// bounded connection. Pipelining is refused instead of allocating a queue.
pub async fn serve_control<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    permit: ConnectionPermit,
    owner: VerifiedOwner,
    _guard: OwnerConnectionGuard,
    source: Arc<dyn AuthorizationSource>,
    backend: Arc<dyn SessionBackend>,
    shutdown: CancellationToken,
) -> Result<(), Error> {
    let owner = Arc::new(owner);
    let permit = Arc::new(permit);
    // A fixed-shape correlated refusal remains writable when operation or
    // normal reply capacity is saturated. Reserved before receiving requests.
    let _refusal_bytes = permit
        .budgets()
        .reserve(Resource::ControlBytes, REFUSAL_BYTES)?;
    #[cfg(feature = "relay")]
    let inventory = Arc::new(std::sync::Mutex::new(None));
    #[cfg(feature = "relay")]
    let _inventory_lifetime = InventoryLifetime(inventory.clone());
    let mut capabilities = None;
    loop {
        #[cfg(feature = "relay")]
        {
            let mut retained = inventory.lock().map_err(|_| ContractError::Denied)?;
            if retained
                .as_ref()
                .is_some_and(|snapshot| std::time::Instant::now() >= snapshot.expires)
            {
                *retained = None;
            }
        }
        let duration = std::time::Duration::from_secs(u64::from(CONTROL_DEADLINE_SECONDS));
        let first = tokio::select! {
            biased;
            _ = shutdown.cancelled() => return Err(Error::Cancelled),
            result = tokio::time::timeout(std::time::Duration::from_secs(CONTROL_IDLE_SECONDS), stream.read_u8()) =>
                result.map_err(|_| Error::Deadline)??,
        };
        let frame = tokio::select! {
            biased;
            _ = shutdown.cancelled() => return Err(Error::Cancelled),
            result = tokio::time::timeout(duration, read_frame_started(&mut stream, permit.budgets(), first)) =>
                result.map_err(|_| Error::Deadline)??,
        };
        let request: Envelope<Request> = frame.decode()?;
        drop(frame);
        let deadline = tokio::time::Instant::now() + duration;
        if capabilities.is_none() && !matches!(request.body, Request::Capabilities {}) {
            return Err(ContractError::UnsupportedVersion.into());
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancel = CancelOperation(cancelled.clone());
        let admission = Arc::new(OperationAdmission {
            owner: owner.clone(),
            source: source.clone(),
            request: request.clone(),
            cancelled,
            #[cfg(feature = "relay")]
            inventory: inventory.clone(),
        });
        let worker_admission = admission.clone();
        let worker_backend = backend.clone();
        let worker_permit = permit.clone();
        let worker = tokio::task::spawn_blocking(move || {
            // Protected file/vault I/O also belongs here. The aggregate client
            // permit bounds workers even when normal operation capacity is full.
            execute(
                worker_backend.as_ref(),
                &worker_admission,
                &worker_permit,
                capabilities,
            )
        });
        let result = tokio::select! {
            biased;
            _ = shutdown.cancelled() => return Err(Error::Cancelled),
            // Any EOF/error retires the owner; data is forbidden pipelining.
            _ = stream.read_u8() => return Err(Error::Closed),
            result = tokio::time::timeout_at(deadline, worker) =>
                result.map_err(|_| Error::Deadline)?.map_err(|_| Error::Closed)??,
        };
        let mut response = Envelope::new(
            request.host.clone(),
            request.operation.clone(),
            result.body.clone(),
        );
        response.issuance_epoch = request.issuance_epoch;
        response.validate_for(&request)?;
        let encoded = festerm_host_protocol::encode(&response)?;
        if result.bytes.is_none() && encoded.len() > REFUSAL_BYTES {
            return Err(ContractError::Limit.into());
        }
        let publish = async {
            stream.write_u32(encoded.len() as u32).await?;
            stream.write_all(&encoded).await?;
            stream.flush().await
        };
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => return Err(Error::Cancelled),
            result = tokio::time::timeout_at(deadline, publish) =>
                result.map_err(|_| Error::Deadline)??,
        }
        if let Response::Capabilities {
            capabilities: supported,
        } = response.body
        {
            capabilities = Some(supported);
        }
    }
}
