//! Unexposed HOST dispatcher core. No relay listener calls this module.
//!
//! Enrollment's pinned-channel exporter and protected native session adapter
//! are mandatory integration gates; a bearer connection is not an owner.

use crate::owner_auth::{OwnerConnectionGuard, OwnerVerifier, VerifiedOwner};
use festerm_host_protocol::{
    AdmissionFence, ContractError, Envelope, Epoch, Refusal, Request, Resource, Response,
    SessionGrant, CONTROL_DEADLINE_SECONDS, CONTROL_FRAME_BYTES,
};
pub use festerm_tunnel_client::host_control::ConnectionPermit;
use festerm_tunnel_client::host_control::{read_frame, write_frame, Error};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio_util::sync::CancellationToken;

#[cfg(feature = "relay")]
pub mod native;

#[cfg(test)]
#[path = "host_control_tests.rs"]
mod tests;

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
    pub fn principal(&self) -> &festerm_host_protocol::PrincipalId {
        self.owner.principal()
    }

    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn check_host(&self) -> Result<(), ContractError> {
        let mut request = self.request.clone();
        request.body = Request::Capabilities {};
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
    fn authorize_current(
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
    #[cfg(feature = "relay")]
    let inventory = Arc::new(std::sync::Mutex::new(None));
    #[cfg(feature = "relay")]
    let _inventory_lifetime = InventoryLifetime(inventory.clone());
    let mut handshaken = false;
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
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_secs(u64::from(CONTROL_DEADLINE_SECONDS));
        let frame = tokio::select! {
            biased;
            _ = shutdown.cancelled() => return Err(Error::Cancelled),
            result = tokio::time::timeout_at(deadline, read_frame(&mut stream, permit.budgets())) =>
                result.map_err(|_| Error::Deadline)??,
        };
        let request: Envelope<Request> = frame.decode()?;
        drop(frame);
        if !handshaken && !matches!(request.body, Request::Capabilities {}) {
            return Err(ContractError::UnsupportedVersion.into());
        }
        let operation = permit.budgets().reserve(Resource::Control, 1)?;
        let bytes = permit
            .budgets()
            .reserve(Resource::ControlBytes, CONTROL_FRAME_BYTES)?;
        let create = matches!(
            request.body,
            Request::Ensure { .. } | Request::Create { .. }
        )
        .then(|| permit.budgets().reserve(Resource::PendingCreate, 1))
        .transpose()?;
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
        admission.check_host()?;
        let worker_admission = admission.clone();
        let worker_backend = backend.clone();
        let worker_request = request.clone();
        let worker = tokio::task::spawn_blocking(move || {
            // Charges live in the worker: timeout/disconnect must not release
            // capacity while blocking IPC is still reconciling.
            let _charges = (operation, bytes, create);
            worker_backend.execute(&worker_request, &worker_admission)
        });
        let result = tokio::select! {
            biased;
            _ = shutdown.cancelled() => return Err(Error::Cancelled),
            // Any EOF/error retires the owner; data is forbidden pipelining.
            _ = stream.read_u8() => return Err(Error::Closed),
            result = tokio::time::timeout_at(deadline, worker) =>
                result.map_err(|_| Error::Deadline)?.map_err(|_| Error::Closed)?,
        };
        admission.check_host()?;
        let response = Envelope::new(
            request.host.clone(),
            request.operation.clone(),
            result.unwrap_or_else(|reason| Response::Refused { reason }),
        );
        response.validate_for(&request)?;
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => return Err(Error::Cancelled),
            result = tokio::time::timeout_at(deadline, write_frame(&mut stream, permit.budgets(), &response)) =>
                result.map_err(|_| Error::Deadline)??,
        }
        handshaken = matches!(response.body, Response::Capabilities { .. }) || handshaken;
    }
}
