//! A fresh ordinary Files channel, separate from Control discovery and sessions.
use super::*;
use crate::{file_client, file_protocol::HostScope};
use std::{
    fmt,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::ReadBuf;

#[derive(Debug)]
pub enum FilesError {
    Host(Failure),
    Files(file_client::Error),
}
impl fmt::Display for FilesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Host(error) => f.write_str(error.message()),
            Self::Files(error) => fmt::Display::fmt(error, f),
        }
    }
}
impl std::error::Error for FilesError {}
impl From<Failure> for FilesError {
    fn from(error: Failure) -> Self {
        Self::Host(error)
    }
}
impl From<file_client::Error> for FilesError {
    fn from(error: file_client::Error) -> Self {
        Self::Files(error)
    }
}

/// Constructed only after ordinary Files authentication. Dropping/closing the
/// protocol stream releases its aggregate connection slot with the transport.
pub struct FilesStream {
    stream: relay::ClientConnection,
    _permit: ConnectionPermit,
}
impl AsyncRead for FilesStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buffer)
    }
}
impl AsyncWrite for FilesStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

/// No reconnect or credential export. Scope is fixed by this channel's greeting.
pub struct Files {
    client: file_client::Client<FilesStream>,
    scope: HostScope,
    info: OrdinaryHostInfo,
}
impl Files {
    pub fn scope(&self) -> &HostScope {
        &self.scope
    }
    pub fn host_info(&self) -> &OrdinaryHostInfo {
        &self.info
    }
    /// Observed Open3 capability only; every mutation still rechecks authority.
    pub fn delete_advertised(&self) -> bool {
        self.client.delete_advertised()
    }
    pub fn client(&mut self) -> &mut file_client::Client<FilesStream> {
        &mut self.client
    }
    /// Transfer ownership without changing the account guard, child lifetime,
    /// host scope, or aggregate connection permit retained by the client.
    pub fn into_client(self) -> file_client::Client<FilesStream> {
        self.client
    }
    /// Call in the worker immediately before publishing a local staged download.
    /// Already admitted OS work cannot be rolled back by a later account change.
    /// This checks local account/lifetime only, never remote liveness or consent.
    pub fn check_current(&mut self) -> Result<(), file_client::Error> {
        self.client.check_current()
    }
}

/// Provider-authorized direct Files opening. None confirms files are disabled
/// on the authenticated host; old/closed/malformed hosts are errors, not Off.
/// A pinned scope is an explicit existing-handle constraint, never a grant.
/// Current guarded transfers negotiate the explicit allowlist namespace when
/// advertised; historical descriptions retain the original Open2 bytes.
pub async fn open_files(
    identity: &Identity,
    locator: &str,
    index: OrdinaryKeyIndex,
    budgets: &Budgets,
    account_guard: OrdinaryAccountGuard,
    request_cancel: &CancellationToken,
    pinned: Option<&HostScope>,
) -> Result<Option<Files>, FilesError> {
    open_files_mode(
        identity,
        locator,
        index,
        budgets,
        account_guard,
        request_cancel,
        pinned,
        false,
        None,
    )
    .await
}
/// Explicit Open3 operation negotiation over the same ordinary Files route.
/// Old helpers refuse; no downgrade, second account lookup or replay occurs.
pub async fn open_files_reviewed(
    identity: &Identity,
    locator: &str,
    index: OrdinaryKeyIndex,
    budgets: &Budgets,
    account_guard: OrdinaryAccountGuard,
    request_cancel: &CancellationToken,
    pinned: Option<&HostScope>,
) -> Result<Option<Files>, FilesError> {
    open_files_mode(
        identity,
        locator,
        index,
        budgets,
        account_guard,
        request_cancel,
        pinned,
        true,
        None,
    )
    .await
}

/// Fresh Files opening constrained to saved stable identity before proof.
/// The expected identity is a negative constraint, never a Files grant.
#[allow(clippy::too_many_arguments)]
pub async fn open_files_pinned(
    identity: &Identity,
    locator: &str,
    index: OrdinaryKeyIndex,
    budgets: &Budgets,
    account_guard: OrdinaryAccountGuard,
    request_cancel: &CancellationToken,
    pinned: Option<&HostScope>,
    expected: &ExpectedHost,
) -> Result<Option<Files>, FilesError> {
    open_files_mode(
        identity,
        locator,
        index,
        budgets,
        account_guard,
        request_cancel,
        pinned,
        false,
        Some(expected),
    )
    .await
}

/// Fresh Files opening constrained to saved stable identity before proof.
/// The expected identity is a negative constraint, never a Files grant.
#[allow(clippy::too_many_arguments)]
pub async fn open_files_pinned_reviewed(
    identity: &Identity,
    locator: &str,
    index: OrdinaryKeyIndex,
    budgets: &Budgets,
    account_guard: OrdinaryAccountGuard,
    request_cancel: &CancellationToken,
    pinned: Option<&HostScope>,
    expected: &ExpectedHost,
) -> Result<Option<Files>, FilesError> {
    open_files_mode(
        identity,
        locator,
        index,
        budgets,
        account_guard,
        request_cancel,
        pinned,
        true,
        Some(expected),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn open_files_mode(
    identity: &Identity,
    locator: &str,
    index: OrdinaryKeyIndex,
    budgets: &Budgets,
    account_guard: OrdinaryAccountGuard,
    request_cancel: &CancellationToken,
    pinned: Option<&HostScope>,
    reviewed: bool,
    expected: Option<&ExpectedHost>,
) -> Result<Option<Files>, FilesError> {
    let source = match expected {
        Some(expected) => KeySource::PinnedBootstrap(index, expected.clone()),
        None => KeySource::Bootstrap(index),
    };
    let Authenticated {
        stream,
        permit,
        info,
        ..
    } = authenticate_fresh_mode(
        identity,
        locator,
        source,
        budgets,
        &account_guard,
        request_cancel,
        StreamRole::Files,
    )
    .await?;
    if reviewed {
        open_authenticated_mode(
            stream,
            permit,
            info,
            account_guard,
            request_cancel,
            pinned,
            true,
        )
        .await
    } else {
        open_authenticated(stream, permit, info, account_guard, request_cancel, pinned).await
    }
}

pub(crate) async fn open_authenticated(
    stream: relay::ClientConnection,
    permit: ConnectionPermit,
    info: OrdinaryHostInfo,
    account_guard: OrdinaryAccountGuard,
    request_cancel: &CancellationToken,
    pinned: Option<&HostScope>,
) -> Result<Option<Files>, FilesError> {
    open_authenticated_mode(
        stream,
        permit,
        info,
        account_guard,
        request_cancel,
        pinned,
        false,
    )
    .await
}

async fn open_authenticated_mode(
    stream: relay::ClientConnection,
    permit: ConnectionPermit,
    info: OrdinaryHostInfo,
    account_guard: OrdinaryAccountGuard,
    request_cancel: &CancellationToken,
    pinned: Option<&HostScope>,
    reviewed: bool,
) -> Result<Option<Files>, FilesError> {
    account_guard.check().map_err(Failure::from)?;
    let lifetime = account_guard.cancel.child_token();
    let stream = FilesStream {
        stream,
        _permit: permit,
    };
    let opened = tokio::select! { biased;
        _ = request_cancel.cancelled() => return Err(Failure::Control(Error::Cancelled).into()),
        _ = account_guard.cancel.cancelled() => return Err(Failure::Control(Error::Cancelled).into()),
        result = async {
            if reviewed {
                file_client::Client::open_host_guarded_reviewed(stream, &info.route.host.host,
                    &info.installation, pinned, account_guard.clone(), lifetime).await
            } else {
                file_client::Client::open_host_guarded(stream, &info.route.host.host,
                    &info.installation, pinned, account_guard.clone(), lifetime).await
            }
        } => result?,
    };
    account_guard.check().map_err(Failure::from)?;
    if request_cancel.is_cancelled() {
        return Err(Failure::Control(Error::Cancelled).into());
    }
    Ok(opened.map(|(client, scope)| Files {
        client,
        scope,
        info,
    }))
}
