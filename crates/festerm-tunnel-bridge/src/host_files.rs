//! HOST-only rooted service composition. Authentication and protected file
//! consent are independent of daemon/session grants and terminal admission.
use crate::{
    file_protocol::{self as rpc, HostPolicy},
    filesystem::{self, Filesystem},
    host_control::{
        AuthorityKind, ConnectionAuthority, PolicyGuard, PolicyView, ProtectedPolicySource,
    },
    owner_auth::ConnectionGuard,
    wire::{read_client_frame, write_frame, DEADLINE},
};
use festerm_host_protocol::ContractError;
use festerm_tunnel_client::host_control::ConnectionPermit;
use std::{io, sync::Arc};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

pub(crate) struct Prepared {
    policy: HostPolicy,
    filesystem: Arc<Filesystem>,
}
impl Prepared {
    /// Native setup only, outside the protected policy lease. Preparing a root
    /// grants no authority: every use must still match current protected consent.
    pub(crate) fn open(policy: HostPolicy) -> io::Result<Arc<Self>> {
        policy.scope.validate()?;
        let root = policy.filesystem.as_ref().ok_or_else(denied)?;
        root.validate()?;
        let filesystem = Filesystem::open(root).map_err(|_| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "host.filesystem_root_unavailable",
            )
        })?;
        Ok(Arc::new(Self { policy, filesystem }))
    }
}
fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "HOST file access is unavailable; review current remote-access settings",
    )
}
fn authorization(error: ContractError) -> io::Error {
    if error == ContractError::Busy {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "HOST file-access settings are busy",
        )
    } else {
        denied()
    }
}
type Current = Arc<dyn Fn() -> io::Result<HostPolicy> + Send + Sync>;
async fn current(read: Current, cancel: &CancellationToken) -> io::Result<HostPolicy> {
    tokio::select! { biased;
        _ = cancel.cancelled() => Err(denied()),
        result = tokio::time::timeout(DEADLINE, tokio::task::spawn_blocking(move || read())) =>
            result.map_err(|_| denied())?.map_err(|_| denied())?,
    }
}

pub(crate) async fn serve<A: ProtectedPolicySource>(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    permit: ConnectionPermit,
    owner: ConnectionAuthority,
    guard: ConnectionGuard,
    source: Arc<A>,
    prepared: Option<Arc<Prepared>>,
    cancel: CancellationToken,
) -> io::Result<()> {
    let _connection_guard = guard;
    let ConnectionAuthority::Ordinary(capability) = owner else {
        return Err(denied());
    };
    // The current-policy closure is invoked only on existing blocking workers.
    // Capturing this permit keeps abandoned admission jobs inside HOST budgets.
    let permit = Arc::new(permit);
    let stopped = cancel.clone();
    let read: Current = Arc::new(move || {
        let _charge = &permit;
        if stopped.is_cancelled() {
            return Err(denied());
        }
        let current = source
            .current_policy_for(AuthorityKind::Ordinary)
            .map_err(authorization)?;
        let PolicyView::Ordinary(policy) = current.view() else {
            return Err(denied());
        };
        let policy = capability
            .authorize_files(policy)
            .map_err(authorization)?
            .clone();
        Ok(policy) // current guard/lease drops before any filesystem/network I/O
    });
    tokio::select! { biased;
        _ = cancel.cancelled() => Err(denied()),
        result = serve_inner(stream, read, prepared, cancel.clone()) => result,
    }
}

async fn serve_inner(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    read: Current,
    prepared: Option<Arc<Prepared>>,
    cancel: CancellationToken,
) -> io::Result<()> {
    let captured = current(read.clone(), &cancel).await?;
    let filesystem = match (&captured.filesystem, prepared) {
        (None, _) => None,
        (Some(_), Some(prepared)) if captured == prepared.policy => {
            Some(prepared.filesystem.clone())
        }
        _ => return Err(denied()),
    };
    write_frame(
        &mut stream,
        rpc::HOST_DESCRIPTION,
        &serde_json::to_vec(&rpc::HostDescription {
            version: rpc::HOST_VERSION,
            scope: captured.scope.clone(),
            filesystem: filesystem.as_ref().map(|root| root.capabilities()),
        })?,
    )
    .await?;
    let Some(filesystem) = filesystem else {
        return Ok(());
    };
    let (kind, bytes) = tokio::time::timeout(DEADLINE, read_client_frame(&mut stream))
        .await
        .map_err(|_| denied())??;
    if kind != rpc::OPEN {
        return Err(denied());
    }
    let open: rpc::HostOpen = serde_json::from_slice(&bytes).map_err(|_| denied())?;
    open.scope.validate()?;
    if !matches!(open.version, rpc::HOST_VERSION | rpc::HOST_REVIEWED_VERSION)
        || open.scope != captured.scope
    {
        return Err(denied());
    }
    let admission: filesystem::Admission = Arc::new(move |_| {
        if read()? != captured {
            return Err(denied());
        }
        Ok(())
    });
    if open.version == rpc::HOST_REVIEWED_VERSION {
        filesystem::serve_authorized_reviewed(stream, filesystem, admission, cancel).await
    } else {
        filesystem::serve_authorized(stream, filesystem, admission, cancel).await
    }
}
