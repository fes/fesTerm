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
pub async fn open_files(
    identity: &Identity,
    locator: &str,
    index: OrdinaryKeyIndex,
    budgets: &Budgets,
    account_guard: OrdinaryAccountGuard,
    request_cancel: &CancellationToken,
    pinned: Option<&HostScope>,
) -> Result<Option<Files>, FilesError> {
    let Authenticated {
        stream,
        permit,
        info,
        ..
    } = authenticate_fresh(
        identity,
        locator,
        index,
        budgets,
        &account_guard,
        request_cancel,
        StreamRole::Files,
    )
    .await?;
    open_authenticated(stream, permit, info, account_guard, request_cancel, pinned).await
}

pub(crate) async fn open_authenticated(
    stream: relay::ClientConnection,
    permit: ConnectionPermit,
    info: OrdinaryHostInfo,
    account_guard: OrdinaryAccountGuard,
    request_cancel: &CancellationToken,
    pinned: Option<&HostScope>,
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
        result = file_client::Client::open_host_guarded(stream, &info.route.host.host,
            &info.installation, pinned, account_guard.clone(), lifetime) => result?,
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
