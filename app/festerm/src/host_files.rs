//! Desktop Files composition. Authentication stays in foreground ordinary HOST
//! preparation; this adapter owns only one already-authorized Files stream.
use festerm_ssh::{
    RemoteDirectoryPage, RemoteReplacementReview, RemoteTransferEndpoint, RemoteTransferFuture,
    SftpDirectoryItem, SftpEntryType, SftpPath, SftpPathMetadata, SftpSessionError,
};
use festerm_tunnel_bridge::{
    file_client::{self, Client, Handle},
    file_protocol as rpc,
    host_client::ordinary::{Files, FilesStream},
};
use std::{
    future::Future,
    pin::Pin,
    time::{Duration, UNIX_EPOCH},
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

#[path = "host_files/reviewed_delete.rs"]
mod reviewed_delete;
pub(crate) use reviewed_delete::{DeleteObservation, DeleteOutcome};
#[path = "host_files/first_failure.rs"]
mod first_failure;
pub(crate) use first_failure::{Channel as FilesChannel, FirstFailure};

pub(crate) struct FilesEndpoint<S> {
    client: Client<S>,
    lifetime: CancellationToken,
    first_failure: FirstFailure,
    channel: FilesChannel,
    locally_retired: bool,
    next: u64,
    generation: uuid::Uuid,
    in_flight_upload: Option<(uuid::Uuid, String)>,
    directory: Option<(u64, Handle, String)>,
    review: Option<(u64, Handle, String)>,
    delete_review: Option<reviewed_delete::PreparedDelete>,
}
impl FilesEndpoint<FilesStream> {
    pub(crate) fn new(
        files: Files,
        lifetime: CancellationToken,
        first_failure: FirstFailure,
        channel: FilesChannel,
    ) -> Self {
        Self {
            client: files.into_client(),
            lifetime,
            first_failure,
            channel,
            locally_retired: false,
            next: 1,
            generation: uuid::Uuid::new_v4(),
            in_flight_upload: None,
            directory: None,
            review: None,
            delete_review: None,
        }
    }
}
impl<S: AsyncRead + AsyncWrite + Unpin> FilesEndpoint<S> {
    fn record_failure(&self, stage: &'static str, error: &file_client::Error) {
        // Dropping an opaque upload intentionally closes this protocol stream.
        // Its later Closed/Cancelled is not a new remote failure.
        if !self.locally_retired
            || !matches!(
                error,
                file_client::Error::Closed | file_client::Error::Cancelled
            )
        {
            self.first_failure.record(self.channel, stage, error);
        }
    }
    fn record_transfer_failure(&mut self, stage: &'static str, error: &file_client::Error) {
        if matches!(
            error,
            file_client::Error::LocalIo(std::io::ErrorKind::Interrupted)
        ) {
            // The existing transfer progress callback uses Interrupted for
            // cancellation. This alone does not identify a remote failure or
            // prove why the local source/sink was interrupted.
            self.locally_retired = !self.client.available();
        } else if !self.client.available() {
            self.record_failure(stage, error);
        }
    }
    fn next_id(&mut self) -> Result<u64, SftpSessionError> {
        let id = self.next;
        self.next = self
            .next
            .checked_add(1)
            .ok_or(SftpSessionError::SessionClosed)?;
        Ok(id)
    }
    fn error(path: &str, operation: &'static str, error: file_client::Error) -> SftpSessionError {
        match error {
            file_client::Error::CleanupFailed { operation: cause, cleanup } => {
                SftpSessionError::TransferFailedWithIncompleteCleanup {
                    operation_error: Box::new(Self::error(path, operation, *cause)),
                    cleanup_error: Box::new(Self::error(path, "close transfer handle", *cleanup)),
                }
            }
            file_client::Error::UncertainFinish(cause) => SftpSessionError::PublicationUncertain {
                path: path.into(),
                reason: cause.to_string(),
            },
            file_client::Error::Remote {
                code: rpc::ErrorCode::RecoveryRequired,
                recovery_path,
            } => SftpSessionError::PublicationUncertain {
                path: path.into(),
                reason: recovery_path.map_or_else(
                    || "Host publication requires recovery; do not retry automatically.".into(),
                    |path| format!("Host publication requires recovery at {path}; do not retry automatically."),
                ),
            },
            file_client::Error::Remote {
                code: rpc::ErrorCode::Exists,
                ..
            } => SftpSessionError::DestinationExists { path: path.into() },
            other => SftpSessionError::RemoteOperationFailed {
                operation,
                path: path.into(),
                reason: other.to_string(),
            },
        }
    }
    fn metadata_value(
        &self,
        path: &str,
        metadata: rpc::Metadata,
    ) -> Result<SftpPathMetadata, SftpSessionError> {
        let size = metadata
            .size
            .parse()
            .map_err(|_| Self::error(path, "read metadata", file_client::Error::Protocol))?;
        let modified_at = metadata
            .modified_ms
            .as_deref()
            .map(|value| {
                let millis = value.parse::<u64>().map_err(|_| {
                    Self::error(path, "read metadata", file_client::Error::Protocol)
                })?;
                UNIX_EPOCH
                    .checked_add(Duration::from_millis(millis))
                    .ok_or_else(|| Self::error(path, "read metadata", file_client::Error::Protocol))
            })
            .transpose()?;
        Ok(SftpPathMetadata {
            path: SftpPath::remote(path),
            file_type: kind(metadata.kind),
            size: Some(size),
            modified_at,
            permissions: Some(if self.client.capabilities().write {
                0o700
            } else {
                0o500
            }),
        })
    }
}
fn kind(value: rpc::Kind) -> SftpEntryType {
    match value {
        rpc::Kind::File => SftpEntryType::File,
        rpc::Kind::Directory => SftpEntryType::Directory,
        rpc::Kind::Unsupported => SftpEntryType::Other,
    }
}
impl<S: AsyncRead + AsyncWrite + Unpin + Send + 'static> RemoteTransferEndpoint
    for FilesEndpoint<S>
{
    fn check_current(&mut self) -> Result<(), SftpSessionError> {
        if self.lifetime.is_cancelled() {
            self.record_failure("authority", &file_client::Error::Cancelled);
            self.client.cancel();
            return Err(SftpSessionError::SessionClosed);
        }
        self.client.check_current().map_err(|error| {
            self.record_failure("authority", &error);
            Self::error("/", "check Files authority", error)
        })
    }
    fn retirement(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let lifetime = self.lifetime.clone();
        Box::pin(async move { lifetime.cancelled().await })
    }
    fn idle_keepalive_interval(&self) -> Option<Duration> {
        Some(Duration::from_secs(20))
    }
    fn keepalive(&mut self) -> RemoteTransferFuture<'_, ()> {
        Box::pin(async move {
            let result = self.ping().await;
            if result.is_err() {
                tracing::warn!(target: "festerm::sftp", channel = self.channel.label(),
                    locally_retired = self.locally_retired,
                    "Files tab retired after liveness failure");
                // Retire only this Files tab's child lifetime. The browsing
                // worker reports ConnectionFailed and drains owned transfers.
                self.lifetime.cancel();
            }
            result
        })
    }
    fn cleanup(&mut self) -> RemoteTransferFuture<'_, ()> {
        Box::pin(async move {
            let interrupted = self.in_flight_upload.take();
            let mut failure = None;
            if let Some((_, handle, path)) = self.directory.take() {
                if let Err(error) = self.client.close(&handle).await {
                    failure = Some(Self::error(&path, "close directory", error));
                }
            }
            if let Some((_, handle, path)) = self.review.take() {
                if let Err(error) = self.client.close(&handle).await {
                    failure = Some(Self::error(&path, "close replacement review", error));
                }
            }
            if let Some((generation, path)) = interrupted {
                self.locally_retired = true;
                self.client.cancel();
                return Err(SftpSessionError::PublicationUncertain { path, reason: format!("The upload future ended before its publication result was observed (connection {generation}). Inspect before starting new work.") });
            }
            if let Some(error) = failure {
                self.client.cancel();
                Err(error)
            } else {
                Ok(())
            }
        })
    }
    fn metadata<'a>(
        &'a mut self,
        path: &'a str,
    ) -> RemoteTransferFuture<'a, Option<SftpPathMetadata>> {
        Box::pin(async move {
            match self.client.stat(path).await {
                Ok(value) => self.metadata_value(path, value).map(Some),
                Err(file_client::Error::Remote {
                    code: rpc::ErrorCode::NotFound,
                    ..
                }) => Ok(None),
                Err(error) => Err(Self::error(path, "stat", error)),
            }
        })
    }
    fn open_directory<'a>(&'a mut self, path: &'a str) -> RemoteTransferFuture<'a, u64> {
        Box::pin(async move {
            if self.directory.is_some() {
                return Err(Self::error(
                    path,
                    "open directory",
                    file_client::Error::Invalid,
                ));
            }
            let id = self.next_id()?;
            let handle = self
                .client
                .open_directory(path)
                .await
                .map_err(|error| Self::error(path, "open directory", error))?;
            self.directory = Some((id, handle, path.into()));
            Ok(id)
        })
    }
    fn read_directory(&mut self, id: u64) -> RemoteTransferFuture<'_, RemoteDirectoryPage> {
        Box::pin(async move {
            let (active, handle, path) = self
                .directory
                .as_ref()
                .ok_or(SftpSessionError::SessionClosed)?;
            if *active != id {
                return Err(SftpSessionError::SessionClosed);
            }
            let page = self
                .client
                .read_directory(handle, self.client.capabilities().page_limit)
                .await
                .map_err(|error| Self::error(path, "read directory", error))?;
            if page.skipped != 0 {
                return Err(SftpSessionError::RemoteOperationFailed {
                    operation: "plan directory transfer",
                    path: path.clone(),
                    reason: "The host omitted names that cannot be represented safely; a complete folder transfer could not be planned.".into(),
                });
            }
            let permissions = Some(if self.client.capabilities().write {
                0o700
            } else {
                0o500
            });
            Ok(RemoteDirectoryPage {
                done: page.done,
                entries: page
                    .entries
                    .into_iter()
                    .map(|entry| {
                        let child = if path == "/" {
                            format!("/{0}", entry.name)
                        } else {
                            format!("{path}/{0}", entry.name)
                        };
                        SftpDirectoryItem {
                            name: entry.name,
                            path: SftpPath::remote(child),
                            file_type: kind(entry.kind),
                            size: None,
                            modified_at: None,
                            permissions,
                        }
                    })
                    .collect(),
            })
        })
    }
    fn close_directory(&mut self, id: u64) -> RemoteTransferFuture<'_, ()> {
        Box::pin(async move {
            if !self
                .directory
                .as_ref()
                .is_some_and(|(active, _, _)| *active == id)
            {
                return Err(SftpSessionError::SessionClosed);
            }
            let (_, handle, path) = self.directory.take().expect("checked directory");
            self.client.close(&handle).await.map_err(|error| {
                self.client.cancel();
                Self::error(&path, "close directory", error)
            })
        })
    }
    fn mkdir<'a>(&'a mut self, path: &'a str) -> RemoteTransferFuture<'a, ()> {
        Box::pin(async move {
            self.client
                .mkdir(path)
                .await
                .map_err(|error| Self::error(path, "create directory", error))
        })
    }
    fn review_replacement<'a>(
        &'a mut self,
        path: &'a str,
    ) -> RemoteTransferFuture<'a, (RemoteReplacementReview, SftpPathMetadata)> {
        Box::pin(async move {
            if self.review.is_some() {
                return Err(Self::error(
                    path,
                    "review replacement",
                    file_client::Error::Invalid,
                ));
            }
            let id = self.next_id()?;
            let (handle, info) = self
                .client
                .open_read(path)
                .await
                .map_err(|error| Self::error(path, "review replacement", error))?;
            let metadata = match self.metadata_value(path, info) {
                Ok(value) => value,
                Err(error) => {
                    self.client.cancel();
                    return Err(error);
                }
            };
            self.review = Some((id, handle, path.into()));
            Ok((RemoteReplacementReview(id), metadata))
        })
    }
    fn release_replacement(&mut self, id: RemoteReplacementReview) -> RemoteTransferFuture<'_, ()> {
        Box::pin(async move {
            if !self
                .review
                .as_ref()
                .is_some_and(|(active, _, _)| *active == id.0)
            {
                return Err(SftpSessionError::SessionClosed);
            }
            let (_, handle, path) = self.review.take().expect("checked replacement");
            self.client.close(&handle).await.map_err(|error| {
                self.client.cancel();
                Self::error(&path, "close replacement review", error)
            })
        })
    }
    fn upload<'a>(
        &'a mut self,
        path: &'a str,
        size: u64,
        expected: Option<RemoteReplacementReview>,
        mut source: &'a mut (dyn AsyncRead + Unpin + Send),
    ) -> RemoteTransferFuture<'a, u64> {
        Box::pin(async move {
            let expected = match expected {
                Some(id) => {
                    let (active, handle, reviewed_path) = self
                        .review
                        .as_ref()
                        .ok_or(SftpSessionError::SessionClosed)?;
                    if *active != id.0 || reviewed_path != path {
                        return Err(SftpSessionError::SessionClosed);
                    }
                    Some(handle)
                }
                None => None,
            };
            self.in_flight_upload = Some((self.generation, path.into()));
            let result = self.client.upload(path, size, expected, &mut source).await;
            if let Err(error) = &result {
                self.record_transfer_failure("upload", error);
            }
            // All returned outcomes retain their actual classification. Only a
            // dropped opaque future leaves the conservative observation marker.
            self.in_flight_upload = None;
            result.map_err(|error| Self::error(path, "upload", error))
        })
    }
    fn download<'a>(
        &'a mut self,
        path: &'a str,
        mut sink: &'a mut (dyn AsyncWrite + Unpin + Send),
    ) -> RemoteTransferFuture<'a, u64> {
        Box::pin(async move {
            let result = self.client.download(path, &mut sink).await;
            if let Err(error) = &result {
                self.record_transfer_failure("download", error);
            }
            result.map_err(|error| Self::error(path, "download", error))
        })
    }
}

/// Worker-only ownership handoff. These streams never enter configuration or
/// the tab's presentation state, and share the retained account lifetime.
pub(crate) struct FilesConnection {
    pub browsing: Files,
    pub transfer: Files,
    pub account_id: u64,
    pub lifetime: CancellationToken,
}
pub(crate) type ConnectFiles = Pin<
    Box<dyn Future<Output = Result<FilesConnection, super::host_browser::RequestFailure>> + Send>,
>;

const FILES_DIRECTORY_ENTRY_LIMIT: usize = 4096;
const FILES_DIRECTORY_BYTE_LIMIT: usize = 4 * 1024 * 1024;
const FILES_DIRECTORY_PAGE_LIMIT: usize = 1024;

impl<S: AsyncRead + AsyncWrite + Unpin + Send + 'static> FilesEndpoint<S> {
    pub(crate) async fn ping(&mut self) -> Result<(), SftpSessionError> {
        self.check_current()?;
        self.client.ping().await.map_err(|error| {
            self.record_failure("liveness", &error);
            Self::error("/", "check Files connection", error)
        })?;
        self.check_current()
    }
    pub(crate) async fn directory_snapshot(
        &mut self,
        path: &str,
    ) -> Result<(festerm_ssh::SftpDirectorySnapshot, SftpPathMetadata, usize), SftpSessionError>
    {
        let metadata = self.metadata(path).await?.ok_or_else(|| {
            Self::error(
                path,
                "list directory",
                file_client::Error::Remote {
                    code: rpc::ErrorCode::NotFound,
                    recovery_path: None,
                },
            )
        })?;
        if metadata.file_type != SftpEntryType::Directory {
            return Err(SftpSessionError::RemotePathNotDirectory { path: path.into() });
        }
        let handle = self.open_directory(path).await?;
        let result = async {
            let mut entries = Vec::new();
            let mut bytes = 0usize;
            let mut skipped = 0usize;
            for _ in 0..FILES_DIRECTORY_PAGE_LIMIT {
                let (_, wire_handle, _) = self.directory.as_ref().ok_or(SftpSessionError::SessionClosed)?;
                let limit = self.client.capabilities().page_limit;
                let page = self.client.read_directory(wire_handle, limit).await.map_err(|error| Self::error(path, "list directory", error))?;
                skipped = skipped.checked_add(usize::from(page.skipped)).ok_or(SftpSessionError::SessionClosed)?;
                for entry in page.entries {
                    let child = if path == "/" { format!("/{}", entry.name) } else { format!("{path}/{}", entry.name) };
                    bytes = bytes.checked_add(child.len() + entry.name.len() + std::mem::size_of::<SftpDirectoryItem>()).ok_or(SftpSessionError::SessionClosed)?;
                    if entries.len() >= FILES_DIRECTORY_ENTRY_LIMIT || bytes > FILES_DIRECTORY_BYTE_LIMIT {
                        return Err(SftpSessionError::RemoteOperationFailed { operation: "list directory", path: path.into(), reason: "Folder exceeds the bounded Files view. No partial listing was published; enter a narrower folder path.".into() });
                    }
                    entries.push(SftpDirectoryItem { name: entry.name, path: SftpPath::remote(child), file_type: kind(entry.kind), size: None, modified_at: None, permissions: metadata.permissions });
                }
                if page.done {
                    self.check_current()?;
                    return Ok((festerm_ssh::SftpDirectorySnapshot { location: festerm_ssh::SftpLocation::Remote, path: SftpPath::remote(path), loaded_at: std::time::SystemTime::now(), entries }, metadata, skipped));
                }
            }
            Err(SftpSessionError::RemoteOperationFailed { operation: "list directory", path: path.into(), reason: "Folder exceeds the Files page limit. No partial listing was published.".into() })
        }.await;
        let closed = self.close_directory(handle).await;
        match (result, closed) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(value), Ok(())) => Ok(value),
        }
    }

    pub(crate) async fn markdown_snapshot(
        &mut self,
        path: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, SftpSessionError> {
        let (handle, info) = self
            .client
            .open_read(path)
            .await
            .map_err(|error| Self::error(path, "open preview", error))?;
        let result = async {
            let size = info
                .size
                .parse::<u64>()
                .map_err(|_| SftpSessionError::SessionClosed)?;
            if size > max_bytes as u64 {
                return Err(SftpSessionError::RemoteFileTooLarge {
                    path: path.into(),
                    limit_bytes: max_bytes,
                });
            }
            self.check_current()?;
            let mut bytes = Vec::with_capacity(size as usize);
            let mut offset = 0;
            loop {
                let length = self.client.capabilities().chunk_limit as u32;
                let chunk = self
                    .client
                    .read(&handle, offset, length)
                    .await
                    .map_err(|error| Self::error(path, "read preview", error))?;
                self.check_current()?;
                if chunk.bytes.len() > max_bytes.saturating_sub(bytes.len()) {
                    return Err(SftpSessionError::RemoteFileTooLarge {
                        path: path.into(),
                        limit_bytes: max_bytes,
                    });
                }
                offset += chunk.bytes.len() as u64;
                bytes.extend_from_slice(&chunk.bytes);
                if chunk.eof {
                    return Ok(bytes);
                }
            }
        }
        .await;
        let closed = self
            .client
            .close(&handle)
            .await
            .map_err(|error| Self::error(path, "close preview", error));
        match (result, closed) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(value), Ok(())) => Ok(value),
        }
    }
}

impl super::FesTermApp {
    pub(super) fn start_configured_files(
        &mut self,
        profile_id: &str,
        context: &eframe::egui::Context,
    ) {
        let Some(profile) = self
            .state
            .configuration()
            .profile(profile_id)
            .and_then(festerm_config::Profile::as_dev_tunnel_files)
        else {
            return;
        };
        let target = crate::sftp_file_manager::HostFilesLaunchTarget {
            label: profile.identifier().into(),
            profile_id: Some(profile.identifier().into()),
            account: profile.account().map(str::to_owned),
            locator: profile.host().into(),
        };
        self.start_host_files(target, context);
    }

    pub(super) fn start_host_files(
        &mut self,
        target: crate::sftp_file_manager::HostFilesLaunchTarget,
        context: &eframe::egui::Context,
    ) {
        use festerm_tunnel_bridge::host_client::{
            self,
            ordinary::{open_files, open_files_reviewed, OrdinaryKeyIndex},
        };
        use festerm_tunnel_client::{host_control::ConnectionPermit, service};
        if !context.input(|input| input.focused) {
            return;
        }
        if festerm_config::Profile::dev_tunnel_files(
            &target.label,
            target.account.clone(),
            &target.locator,
        )
        .is_err()
        {
            self.secure_storage_feedback = Some(
                "The Files destination metadata is invalid. Review its name, account and host.",
            );
            return;
        }
        if service::locator(&target.locator).is_err() {
            self.secure_storage_feedback =
                Some("The selected Files host is invalid. Choose it again.");
            return;
        }
        if let Some(profile_id) = &target.profile_id {
            let current = self
                .state
                .configuration()
                .profile(profile_id)
                .and_then(festerm_config::Profile::as_dev_tunnel_files)
                .is_some_and(|profile| {
                    profile.host() == target.locator
                        && profile.account() == target.account.as_deref()
                });
            if !current {
                self.secure_storage_feedback =
                    Some("The Files profile changed. Review it again before connecting.");
                return;
            }
        }
        let Some(account) = target
            .account
            .as_deref()
            .and_then(|key| self.state.configuration().account(key))
            .cloned()
        else {
            self.secure_storage_feedback =
                Some("Choose a signed-in account in the Files profile before connecting.");
            return;
        };
        let store = match &self.secret_store {
            Ok(store) => store.clone(),
            Err(_) => {
                self.secure_storage_feedback =
                    Some("Native secure storage is unavailable. No Files fallback was attempted.");
                return;
            }
        };
        let account_cancel = match self.state.ordinary_account_token(&account) {
            Ok(token) => token,
            Err(message) => {
                self.secure_storage_feedback = Some(message);
                return;
            }
        };
        let permit = match ConnectionPermit::reserve(&self.state.host_client_budgets) {
            Ok(permit) => std::sync::Arc::new(permit),
            Err(_) => {
                self.secure_storage_feedback =
                    Some("The HOST connection budget is busy. Retry Files explicitly.");
                return;
            }
        };
        let lifetime = account_cancel.child_token();
        let request = lifetime.clone();
        let owner_cancel = lifetime.clone();
        let budgets = self.state.host_client_budgets.clone();
        let locator = target.locator.clone();
        let wake = context.clone();
        let connect: ConnectFiles = Box::pin(async move {
            if request.is_cancelled() {
                return Err("Files connection cancelled.".into());
            }
            let (identity, guard) = super::host_browser::prepare_ordinary_account(
                &account,
                store.as_ref(),
                account_cancel,
                permit,
                super::host_browser::PrivacyPreparationDiagnostic::new(1),
                request.clone(),
                wake,
                #[cfg(test)]
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )?;
            if request.is_cancelled() {
                return Err("Files connection cancelled.".into());
            }
            // Closing this tab fences only its two streams. The retained
            // parent guard still observes account/privacy retirement, while
            // closing one protocol client cannot cancel its sibling or account.
            let parent_guard = guard;
            let guard = host_client::ordinary::OrdinaryAccountGuard::new(
                request.clone(),
                std::sync::Arc::new(move || parent_guard.check()),
            );
            let index = OrdinaryKeyIndex::native()
                .map_err(|error| super::host_browser::RequestFailure::from(error.message()))?;
            let browsing = open_files_reviewed(
                &identity,
                &locator,
                index.clone(),
                &budgets,
                guard.clone(),
                &request,
                None,
            )
            .await
            .map_err(|error| super::host_browser::RequestFailure::from(error.to_string()))?
            .ok_or_else(|| {
                super::host_browser::RequestFailure::from(
                    "Files are disabled on the authenticated host.",
                )
            })?;
            let scope = browsing.scope().clone();
            let transfer = open_files(
                &identity,
                &locator,
                index,
                &budgets,
                guard.clone(),
                &request,
                Some(&scope),
            )
            .await
            .map_err(|error| super::host_browser::RequestFailure::from(error.to_string()))?
            .ok_or_else(|| {
                super::host_browser::RequestFailure::from(
                    "Files were disabled while opening the transfer stream.",
                )
            })?;
            guard
                .check()
                .map_err(|_| "OS privacy or account state changed.")?;
            if request.is_cancelled() {
                return Err("Files connection cancelled.".into());
            }
            Ok(FilesConnection {
                browsing,
                transfer,
                account_id: account.account_id(),
                lifetime: request,
            })
        });
        let settings = self.state.configuration().interface_settings();
        let local = settings
            .default_sftp_local_directory()
            .map(std::path::Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(crate::sftp_file_manager::local_home_directory);
        let tab = crate::sftp_file_manager::SftpFileManagerTab::new_files(
            target,
            connect,
            owner_cancel,
            local,
            settings.sftp_pane_order(),
            context,
        );
        self.state.place_sftp_file_manager(tab);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_ssh::{SftpTransferManager, SftpTransferRequest};
    use festerm_tunnel_bridge::{
        protocol::Target,
        wire::{read_frame, write_frame},
    };
    use tokio::io::{AsyncReadExt, DuplexStream};

    fn limits() -> rpc::Capabilities {
        rpc::Capabilities {
            version: rpc::VERSION,
            write: true,
            chunk_limit: rpc::CHUNK_LIMIT,
            page_limit: rpc::PAGE_LIMIT,
            handle_limit: rpc::HANDLE_LIMIT,
        }
    }
    async fn reply(
        peer: &mut DuplexStream,
        id: u32,
        result: Option<rpc::Result>,
        error: Option<rpc::ErrorCode>,
    ) {
        write_frame(
            peer,
            rpc::RESPONSE,
            &serde_json::to_vec(&rpc::Response {
                id,
                result,
                error,
                recovery_path: None,
            })
            .unwrap(),
        )
        .await
        .unwrap();
    }
    async fn opened(peer: &mut DuplexStream) {
        assert_eq!(read_frame(peer).await.unwrap().0, rpc::OPEN);
        reply(
            peer,
            0,
            Some(rpc::Result::Ready {
                capabilities: limits(),
            }),
            None,
        )
        .await;
    }
    async fn request(peer: &mut DuplexStream) -> rpc::Request {
        let (kind, bytes) = read_frame(peer).await.unwrap();
        assert_eq!(kind, rpc::REQUEST);
        serde_json::from_slice(&bytes).unwrap()
    }
    async fn endpoint(
        stream: DuplexStream,
        lifetime: CancellationToken,
    ) -> FilesEndpoint<DuplexStream> {
        let client = Client::open(
            stream,
            Target {
                name: "owned-files-adapter".into(),
                pid: std::process::id(),
                generation: "123".into(),
            },
            lifetime.clone(),
        )
        .await
        .unwrap();
        FilesEndpoint {
            client,
            lifetime,
            first_failure: FirstFailure::default(),
            channel: FilesChannel::Transfer,
            locally_retired: false,
            next: 1,
            generation: uuid::Uuid::nil(),
            in_flight_upload: None,
            directory: None,
            review: None,
            delete_review: None,
        }
    }
    async fn joined(task: tokio::task::JoinHandle<()>) {
        tokio::time::timeout(std::time::Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn files_transfer_command_arriving_during_ping_does_not_drop_exchange() {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let (ping_sent, ping_seen) = tokio::sync::oneshot::channel();
        let (continue_ping, wait_ping) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            opened(&mut peer).await;
            let ping = request(&mut peer).await;
            assert!(matches!(ping.operation, rpc::Operation::Ping {}));
            ping_sent.send(()).unwrap();
            wait_ping.await.unwrap();
            reply(&mut peer, ping.id, Some(rpc::Result::Pong), None).await;
            let stat = request(&mut peer).await;
            assert!(matches!(stat.operation, rpc::Operation::Stat { .. }));
            reply(&mut peer, stat.id, None, Some(rpc::ErrorCode::NotFound)).await;
            let mut eof = [0];
            assert_eq!(peer.read(&mut eof).await.unwrap(), 0);
        });
        let lifetime = CancellationToken::new();
        let mut manager =
            SftpTransferManager::from_remote_endpoint(endpoint(stream, lifetime.clone()).await);
        tokio::task::yield_now().await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(20)).await;
        tokio::time::resume();
        tokio::time::timeout(Duration::from_secs(3), ping_seen)
            .await
            .unwrap()
            .unwrap();
        let folder = tempfile::tempdir().unwrap();
        manager
            .enqueue_batch(vec![SftpTransferRequest::new(
                SftpPath::remote("/missing"),
                SftpPath::local(folder.path().join("target")),
            )
            .unwrap()])
            .unwrap();
        // The command is ready while the Ping reply remains withheld.
        tokio::task::yield_now().await;
        continue_ping.send(()).unwrap();
        let reason = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let festerm_ssh::SftpTransferEvent::ItemFailed { reason, .. } =
                    manager.recv_event().await.unwrap()
                {
                    break reason;
                }
            }
        })
        .await
        .unwrap();
        assert!(!reason.contains("transport"), "{reason}");
        assert!(!lifetime.is_cancelled());
        manager.shutdown().await.unwrap();
        joined(server).await;
    }

    #[tokio::test]
    async fn files_first_transfer_failure_survives_secondary_browsing_cancellation() {
        for timeout in [false, true] {
            let (transfer_stream, mut transfer_peer) = tokio::io::duplex(8192);
            let (browsing_stream, mut browsing_peer) = tokio::io::duplex(8192);
            let (transfer_seen, transfer_received) = tokio::sync::oneshot::channel();
            let (browsing_seen, browsing_received) = tokio::sync::oneshot::channel();
            let (release, released) = tokio::sync::oneshot::channel();
            let mut release = Some(release);
            let transfer_server = tokio::spawn(async move {
                opened(&mut transfer_peer).await;
                assert!(matches!(
                    request(&mut transfer_peer).await.operation,
                    rpc::Operation::Ping {}
                ));
                transfer_seen.send(()).unwrap();
                let _ = released.await;
                // EOF (or a withheld reply) is the actual primary wire failure.
            });
            let browsing_server = tokio::spawn(async move {
                opened(&mut browsing_peer).await;
                assert!(matches!(
                    request(&mut browsing_peer).await.operation,
                    rpc::Operation::Ping {}
                ));
                browsing_seen.send(()).unwrap();
                let mut byte = [0];
                assert_eq!(browsing_peer.read(&mut byte).await.unwrap(), 0);
            });
            let parent = CancellationToken::new();
            let sibling = parent.child_token();
            let lifetime = parent.child_token();
            let failure = FirstFailure::default();
            let mut transfer = endpoint(transfer_stream, lifetime.child_token()).await;
            transfer.lifetime = lifetime.clone();
            transfer.first_failure = failure.clone();
            let mut browsing = endpoint(browsing_stream, lifetime.child_token()).await;
            browsing.lifetime = lifetime.clone();
            browsing.channel = FilesChannel::Browsing;
            browsing.first_failure = failure.clone();
            assert!(transfer.client.available() && browsing.client.available());
            if timeout {
                tokio::time::pause();
            }
            let transfer_task = tokio::spawn(async move { transfer.keepalive().await });
            transfer_received.await.unwrap();
            // Browsing has its own later deadline, so only transfer can time out.
            if timeout {
                tokio::time::advance(Duration::from_secs(15)).await;
            }
            let browsing_task = tokio::spawn(async move { browsing.ping().await });
            browsing_received.await.unwrap();
            if timeout {
                tokio::time::advance(Duration::from_secs(15) + Duration::from_millis(1)).await;
            } else {
                release.take().unwrap().send(()).unwrap();
            }
            assert!(transfer_task.await.unwrap().is_err());
            let secondary = browsing_task.await.unwrap().unwrap_err().to_string();
            assert!(secondary.contains("cancelled"), "{secondary}");
            let details = failure.details_or(secondary);
            assert!(details.contains("file transfer connection"), "{details}");
            assert!(
                details.contains(if timeout { "timed out" } else { "interrupted" }),
                "{details}"
            );
            assert!(
                !details.contains("cancelled"),
                "secondary cancellation replaced the primary: {details}"
            );
            assert!(lifetime.is_cancelled());
            assert!(!parent.is_cancelled() && !sibling.is_cancelled());
            if timeout {
                tokio::time::resume();
                drop(release);
            }
            joined(transfer_server).await;
            joined(browsing_server).await;
        }
    }

    #[tokio::test]
    async fn files_stalled_write_keeps_deadline_before_later_closed_ping() {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let (seen, received) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            opened(&mut peer).await;
            let begin = request(&mut peer).await;
            assert!(matches!(
                begin.operation,
                rpc::Operation::BeginUpload { .. }
            ));
            reply(
                &mut peer,
                begin.id,
                Some(rpc::Result::Upload { handle: 1 }),
                None,
            )
            .await;
            let write = request(&mut peer).await;
            assert!(matches!(
                write.operation,
                rpc::Operation::Write { length: 1, .. }
            ));
            assert_eq!(read_frame(&mut peer).await.unwrap().0, rpc::DATA);
            seen.send(()).unwrap();
            let mut byte = [0];
            assert_eq!(peer.read(&mut byte).await.unwrap(), 0);
        });
        let lifetime = CancellationToken::new();
        let mut transfer = endpoint(stream, lifetime.child_token()).await;
        transfer.lifetime = lifetime.clone();
        let first = transfer.first_failure.clone();
        tokio::time::pause();
        let task = tokio::spawn(async move {
            let mut source = &b"x"[..];
            let result = transfer.upload("/owned-new", 1, None, &mut source).await;
            (transfer, result)
        });
        received.await.unwrap();
        tokio::time::advance(Duration::from_secs(30) + Duration::from_millis(1)).await;
        let (mut transfer, result) = task.await.unwrap();
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("deadline exceeded"));
        let primary = first.details_or("missing".into());
        assert!(
            primary.contains("transfer") && primary.contains("timed out"),
            "{primary}"
        );
        assert!(transfer.keepalive().await.is_err());
        assert_eq!(first.details_or("secondary".into()), primary);
        assert!(lifetime.is_cancelled());
        tokio::time::resume();
        joined(server).await;
    }

    #[tokio::test]
    async fn files_cancelled_upload_does_not_mask_later_browsing_failure() {
        let (transfer_stream, mut peer) = tokio::io::duplex(8192);
        let (seen, received) = tokio::sync::oneshot::channel();
        let (finish_write, await_write) = tokio::sync::oneshot::channel();
        let (keepalive_seen, keepalive_received) = tokio::sync::oneshot::channel();
        let transfer_server = tokio::spawn(async move {
            opened(&mut peer).await;
            for _ in 0..8 {
                let call = request(&mut peer).await;
                match call.operation {
                    rpc::Operation::Stat { .. } => {
                        reply(&mut peer, call.id, None, Some(rpc::ErrorCode::NotFound)).await
                    }
                    rpc::Operation::BeginUpload { .. } => {
                        reply(
                            &mut peer,
                            call.id,
                            Some(rpc::Result::Upload { handle: 1 }),
                            None,
                        )
                        .await
                    }
                    rpc::Operation::Write { offset, length, .. } => {
                        assert_eq!(read_frame(&mut peer).await.unwrap().0, rpc::DATA);
                        seen.send(()).unwrap();
                        await_write.await.unwrap();
                        reply(
                            &mut peer,
                            call.id,
                            Some(rpc::Result::Written {
                                offset: (offset.parse::<u64>().unwrap() + u64::from(length))
                                    .to_string(),
                            }),
                            None,
                        )
                        .await;
                        let close = request(&mut peer).await;
                        assert!(matches!(
                            close.operation,
                            rpc::Operation::Close { handle: 1 }
                        ));
                        reply(&mut peer, close.id, Some(rpc::Result::Closed), None).await;
                        let ping = request(&mut peer).await;
                        assert!(matches!(ping.operation, rpc::Operation::Ping {}));
                        reply(&mut peer, ping.id, Some(rpc::Result::Pong), None).await;
                        keepalive_seen.send(()).unwrap();
                        let mut byte = [0];
                        assert_eq!(peer.read(&mut byte).await.unwrap(), 0);
                        return;
                    }
                    other => panic!("unexpected operation: {other:?}"),
                }
            }
            panic!("write was not observed");
        });
        let (stream, mut peer) = tokio::io::duplex(8192);
        let browsing_server = tokio::spawn(async move {
            opened(&mut peer).await;
            let ping = request(&mut peer).await;
            reply(&mut peer, ping.id, Some(rpc::Result::Pong), None).await;
            assert!(matches!(
                request(&mut peer).await.operation,
                rpc::Operation::Ping {}
            ));
            // A subsequent genuine browsing EOF must be the first failure.
        });
        let parent = CancellationToken::new();
        let lifetime = parent.child_token();
        let first = FirstFailure::default();
        let mut browsing = endpoint(stream, lifetime.child_token()).await;
        browsing.lifetime = lifetime.clone();
        browsing.channel = FilesChannel::Browsing;
        browsing.first_failure = first.clone();
        let mut transfer = endpoint(transfer_stream, lifetime.child_token()).await;
        transfer.lifetime = lifetime.clone();
        transfer.first_failure = first.clone();
        let mut manager = SftpTransferManager::from_remote_endpoint(transfer);
        let folder = tempfile::tempdir().unwrap();
        let source = folder.path().join("owned-source");
        std::fs::write(&source, b"public contents").unwrap();
        manager
            .enqueue_batch(vec![SftpTransferRequest::new(
                SftpPath::local(&source),
                SftpPath::remote("/owned-new"),
            )
            .unwrap()])
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), received)
            .await
            .unwrap()
            .unwrap();
        manager.cancel_all_transfers().unwrap();
        finish_write.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut cancelled = false;
            loop {
                match manager.recv_event().await.unwrap() {
                    festerm_ssh::SftpTransferEvent::ItemCancelled { .. } => cancelled = true,
                    festerm_ssh::SftpTransferEvent::BatchFinished { .. } => {
                        assert!(cancelled);
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(first.details_or("no failure".into()), "no failure");
        assert!(!lifetime.is_cancelled() && !parent.is_cancelled());
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(20)).await;
        keepalive_received.await.unwrap();
        tokio::time::resume();
        assert!(
            !lifetime.is_cancelled(),
            "confirmed cancellation must keep the tab alive"
        );
        browsing.ping().await.unwrap();
        assert!(browsing.ping().await.is_err());
        let details = first.details_or("missing".into());
        assert!(
            details.contains("browsing") && details.contains("interrupted"),
            "{details}"
        );
        assert!(!details.contains("cancelled"));
        // A later genuine browsing failure still retires this tab, preserving
        // that original failure rather than blaming the earlier cancellation.
        assert!(browsing.keepalive().await.is_err());
        tokio::time::timeout(Duration::from_secs(3), lifetime.cancelled())
            .await
            .unwrap();
        assert!(!parent.is_cancelled());
        assert_eq!(first.details_or("secondary".into()), details);
        manager.shutdown().await.unwrap();
        assert_eq!(std::fs::read(source).unwrap(), b"public contents");
        joined(transfer_server).await;
        joined(browsing_server).await;
    }

    #[tokio::test]
    async fn files_parent_retirement_is_unattributed_and_completed_clients_stay_live() {
        async fn peer(mut peer: DuplexStream) {
            opened(&mut peer).await;
            for _ in 0..2 {
                let ping = request(&mut peer).await;
                assert!(matches!(ping.operation, rpc::Operation::Ping {}));
                reply(&mut peer, ping.id, Some(rpc::Result::Pong), None).await;
            }
            let mut byte = [0];
            assert_eq!(peer.read(&mut byte).await.unwrap(), 0);
        }
        let (a, ap) = tokio::io::duplex(8192);
        let (b, bp) = tokio::io::duplex(8192);
        let servers = [tokio::spawn(peer(ap)), tokio::spawn(peer(bp))];
        let parent = CancellationToken::new();
        let lifetime = parent.child_token();
        let first = FirstFailure::default();
        let mut browsing = endpoint(a, lifetime.child_token()).await;
        browsing.lifetime = lifetime.clone();
        browsing.channel = FilesChannel::Browsing;
        browsing.first_failure = first.clone();
        let mut transfer = endpoint(b, lifetime.child_token()).await;
        transfer.lifetime = lifetime.clone();
        transfer.first_failure = first.clone();
        // The successful open futures and temporary clones have completed.
        // Both retained protocol clients must still work, independently.
        for _ in 0..2 {
            browsing.ping().await.unwrap();
            transfer.keepalive().await.unwrap();
        }
        assert_eq!(first.details_or("no failure".into()), "no failure");
        assert!(!lifetime.is_cancelled());
        parent.cancel();
        assert!(browsing.ping().await.is_err());
        assert!(transfer.keepalive().await.is_err());
        let details = first.details_or("secondary".into());
        assert!(details.contains("browsing") && details.contains("interrupted"));
        assert!(!details.contains("privacy") && !details.contains("user"));
        for server in servers {
            joined(server).await;
        }
    }

    #[tokio::test]
    async fn files_explicit_collision_cancel_is_not_a_liveness_failure() {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            opened(&mut peer).await;
            let stat = request(&mut peer).await;
            reply(
                &mut peer,
                stat.id,
                Some(rpc::Result::Stat {
                    metadata: rpc::Metadata {
                        kind: rpc::Kind::File,
                        size: "1".into(),
                        modified_ms: None,
                    },
                }),
                None,
            )
            .await;
            let mut byte = [0];
            assert_eq!(peer.read(&mut byte).await.unwrap(), 0);
        });
        let lifetime = CancellationToken::new();
        let endpoint = endpoint(stream, lifetime.child_token()).await;
        let first = endpoint.first_failure.clone();
        let mut manager = SftpTransferManager::from_remote_endpoint(endpoint);
        let folder = tempfile::tempdir().unwrap();
        let destination = folder.path().join("existing");
        std::fs::write(&destination, b"original").unwrap();
        manager
            .enqueue_batch(vec![SftpTransferRequest::new(
                SftpPath::remote("/source"),
                SftpPath::local(&destination),
            )
            .unwrap()])
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if matches!(
                    manager.recv_event().await.unwrap(),
                    festerm_ssh::SftpTransferEvent::Collision(_)
                ) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        manager.cancel_all_transfers().unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut cancelled = false;
            loop {
                match manager.recv_event().await.unwrap() {
                    festerm_ssh::SftpTransferEvent::ItemCancelled { .. } => cancelled = true,
                    festerm_ssh::SftpTransferEvent::BatchFinished { .. } => {
                        assert!(cancelled);
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            first.details_or("explicit cancellation".into()),
            "explicit cancellation"
        );
        assert!(!lifetime.is_cancelled());
        assert_eq!(std::fs::read(destination).unwrap(), b"original");
        manager.shutdown().await.unwrap();
        joined(server).await;
    }

    #[tokio::test]
    async fn files_transfer_failed_idle_ping_cancels_collision_and_only_its_child_lifetime() {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            opened(&mut peer).await;
            let stat = request(&mut peer).await;
            assert!(matches!(stat.operation, rpc::Operation::Stat { .. }));
            reply(
                &mut peer,
                stat.id,
                Some(rpc::Result::Stat {
                    metadata: rpc::Metadata {
                        kind: rpc::Kind::File,
                        size: "1".into(),
                        modified_ms: None,
                    },
                }),
                None,
            )
            .await;
            let ping = request(&mut peer).await;
            assert!(matches!(ping.operation, rpc::Operation::Ping {}));
            // Lose the reply without replay, reconnection, or a new grant.
        });
        let parent = CancellationToken::new();
        let sibling = parent.child_token();
        let lifetime = parent.child_token();
        let mut manager =
            SftpTransferManager::from_remote_endpoint(endpoint(stream, lifetime.clone()).await);
        let folder = tempfile::tempdir().unwrap();
        let destination = folder.path().join("existing");
        std::fs::write(&destination, b"original").unwrap();
        manager
            .enqueue_batch(vec![SftpTransferRequest::new(
                SftpPath::remote("/source"),
                SftpPath::local(&destination),
            )
            .unwrap()])
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if matches!(
                    manager.recv_event().await.unwrap(),
                    festerm_ssh::SftpTransferEvent::Collision(_)
                ) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(20)).await;
        tokio::time::resume();
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut cancelled = false;
            loop {
                match manager.recv_event().await.unwrap() {
                    festerm_ssh::SftpTransferEvent::ItemCancelled { .. } => cancelled = true,
                    festerm_ssh::SftpTransferEvent::BatchFinished { .. } => {
                        assert!(cancelled);
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert!(lifetime.is_cancelled());
        assert!(!parent.is_cancelled() && !sibling.is_cancelled());
        tokio::task::yield_now().await;
        assert!(
            manager.snapshot().items.is_empty(),
            "collision reservations drained"
        );
        manager.shutdown().await.unwrap();
        joined(server).await;
        assert_eq!(std::fs::read(destination).unwrap(), b"original");
    }

    // Use the actual rooted server and its unchanged 30-second wire deadline.
    // Advancing only idle time keeps filesystem jobs on the normal clock.
    #[tokio::test]
    async fn files_transfer_stays_live_while_browsing_beyond_server_idle_deadline() {
        use festerm_tunnel_bridge::filesystem::{self, Filesystem};
        #[cfg(not(windows))]
        let parents = [std::env::current_dir().unwrap()];
        #[cfg(windows)]
        let parents = [
            std::env::temp_dir(),
            std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("Temp"),
        ];
        let root = parents
            .into_iter()
            .find_map(|parent| {
                let path = parent.join(format!("files-idle-transfer-{}", uuid::Uuid::new_v4()));
                festerm_tunnel_bridge::setup_storage::private_directory(&path, true)
                    .ok()
                    .map(|_| path)
            })
            .expect("a policy-compliant private fixture root");
        std::fs::write(root.join("download.txt"), b"controlled download").unwrap();
        let filesystem = Filesystem::open(&rpc::Policy {
            root: root.clone(),
            write: false,
            delete: false,
        })
        .unwrap();
        let lifetime = CancellationToken::new();
        let mut servers = Vec::new();
        let mut endpoints = Vec::new();
        for _ in 0..2 {
            let (stream, mut peer) = tokio::io::duplex(8192);
            let filesystem = filesystem.clone();
            servers.push(tokio::spawn(async move {
                let (kind, bytes) = read_frame(&mut peer).await.unwrap();
                assert_eq!(kind, rpc::OPEN);
                let expected: rpc::Open = serde_json::from_slice(&bytes).unwrap();
                filesystem::serve(peer, filesystem, expected.target, &bytes).await
            }));
            endpoints.push(endpoint(stream, lifetime.clone()).await);
        }
        let mut browsing = endpoints.remove(0);
        let mut manager = SftpTransferManager::from_remote_endpoint(endpoints.remove(0));
        tokio::task::yield_now().await;
        for _ in 0..4 {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(10)).await;
            tokio::time::resume();
            browsing.ping().await.unwrap();
            // Let an idle ping finish its real filesystem job before advancing
            // another idle interval. This is not a protocol timeout override.
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            browsing
                .markdown_snapshot("/download.txt", 1024)
                .await
                .unwrap(),
            b"controlled download"
        );
        assert!(
            !lifetime.is_cancelled(),
            "closing a preview retains the tab lifetime"
        );
        let local = tempfile::tempdir().unwrap();
        let destination = local.path().join("download.txt");
        manager
            .enqueue_batch(vec![SftpTransferRequest::new(
                SftpPath::remote("/download.txt"),
                SftpPath::local(&destination),
            )
            .unwrap()])
            .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match manager.recv_event().await.unwrap() {
                    festerm_ssh::SftpTransferEvent::ItemCompleted {
                        bytes_transferred, ..
                    } => break Ok(bytes_transferred),
                    festerm_ssh::SftpTransferEvent::ItemFailed { reason, .. } => break Err(reason),
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        manager.shutdown().await.unwrap();
        browsing.client.cancel();
        for server in servers {
            let _ = server.await.unwrap();
        }
        drop(filesystem);
        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(
            result,
            Ok(19),
            "first transfer STAT must survive idle browsing"
        );
        assert_eq!(std::fs::read(destination).unwrap(), b"controlled download");
    }

    #[tokio::test]
    async fn files_endpoint_owner_shutdown_during_actual_finish_is_uncertain_without_replay() {
        let folder = tempfile::tempdir().unwrap();
        let source = folder.path().join("owned-source");
        std::fs::write(&source, b"new contents").unwrap();
        let (stream, mut peer) = tokio::io::duplex(8192);
        let (finish_sent, finish_seen) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            opened(&mut peer).await;
            for _ in 0..16 {
                let call = request(&mut peer).await;
                match call.operation {
                    rpc::Operation::Stat { .. } => {
                        reply(&mut peer, call.id, None, Some(rpc::ErrorCode::NotFound)).await
                    }
                    rpc::Operation::BeginUpload { expected: None, .. } => {
                        reply(
                            &mut peer,
                            call.id,
                            Some(rpc::Result::Upload { handle: 1 }),
                            None,
                        )
                        .await
                    }
                    rpc::Operation::Write { offset, length, .. } => {
                        let (kind, bytes) = read_frame(&mut peer).await.unwrap();
                        assert_eq!(kind, rpc::DATA);
                        assert_eq!(u32::from_be_bytes(bytes[..4].try_into().unwrap()), call.id);
                        assert_eq!(&bytes[4..], b"new contents");
                        assert_eq!(length, 12);
                        reply(
                            &mut peer,
                            call.id,
                            Some(rpc::Result::Written {
                                offset: (offset.parse::<u64>().unwrap() + u64::from(length))
                                    .to_string(),
                            }),
                            None,
                        )
                        .await;
                    }
                    rpc::Operation::FinishUpload { .. } => {
                        finish_sent.send(()).unwrap();
                        // The actual Finish request is delivered, but its result
                        // is withheld until the queue's owned shutdown drops it.
                        let mut eof = [0];
                        assert_eq!(peer.read(&mut eof).await.unwrap(), 0);
                        return;
                    }
                    other => panic!("unexpected operation: {other:?}"),
                }
            }
            panic!("bounded fixture never observed Finish");
        });
        let endpoint = endpoint(stream, CancellationToken::new()).await;
        let manager = SftpTransferManager::from_remote_endpoint(endpoint);
        manager
            .enqueue_batch(vec![SftpTransferRequest::new(
                SftpPath::local(&source),
                SftpPath::remote("/target"),
            )
            .unwrap()])
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), finish_seen)
            .await
            .unwrap()
            .unwrap();
        let result = manager.shutdown().await;
        assert!(
            matches!(result, Err(SftpSessionError::PublicationUncertain { ref path, .. }) if path == "/target")
        );
        joined(server).await;
        assert_eq!(std::fs::read(source).unwrap(), b"new contents");
    }

    #[tokio::test]
    async fn files_endpoint_returned_refusal_clears_marker_and_preserves_fresh_review() {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            opened(&mut peer).await;
            let begin = request(&mut peer).await;
            assert!(matches!(
                begin.operation,
                rpc::Operation::BeginUpload { .. }
            ));
            reply(&mut peer, begin.id, None, Some(rpc::ErrorCode::Exists)).await;
            let review = request(&mut peer).await;
            assert!(matches!(review.operation, rpc::Operation::OpenRead { .. }));
            reply(
                &mut peer,
                review.id,
                Some(rpc::Result::File {
                    handle: 7,
                    metadata: rpc::Metadata {
                        kind: rpc::Kind::File,
                        size: "3".into(),
                        modified_ms: None,
                    },
                }),
                None,
            )
            .await;
            let close = request(&mut peer).await;
            assert!(matches!(
                close.operation,
                rpc::Operation::Close { handle: 7 }
            ));
            reply(&mut peer, close.id, Some(rpc::Result::Closed), None).await;
            let ping = request(&mut peer).await;
            assert!(matches!(ping.operation, rpc::Operation::Ping {}));
            reply(&mut peer, ping.id, Some(rpc::Result::Pong), None).await;
            let mut eof = [0];
            assert_eq!(peer.read(&mut eof).await.unwrap(), 0);
        });
        let mut endpoint = endpoint(stream, CancellationToken::new()).await;
        let mut source = std::io::Cursor::new(b"new");
        assert!(matches!(
            endpoint.upload("/target", 3, None, &mut source).await,
            Err(SftpSessionError::DestinationExists { .. })
        ));
        assert_eq!(source.position(), 0);
        assert!(endpoint.in_flight_upload.is_none());
        let (review, metadata) = endpoint.review_replacement("/target").await.unwrap();
        assert_eq!(metadata.size, Some(3));
        endpoint.release_replacement(review).await.unwrap();
        endpoint.ping().await.unwrap();
        assert_eq!(
            endpoint.first_failure.details_or("no failure".into()),
            "no failure"
        );
        endpoint.cleanup().await.unwrap();
        endpoint.client.cancel();
        joined(server).await;
    }

    #[tokio::test]
    async fn files_queue_finish_creation_race_requires_fresh_review_without_replay() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("owned-source");
        std::fs::write(&source, b"new").unwrap();
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            opened(&mut peer).await;
            let mut collided = false;
            let mut finished = false;
            for _ in 0..32 {
                let call = request(&mut peer).await;
                match call.operation {
                    rpc::Operation::Stat { .. } if !collided => {
                        reply(&mut peer, call.id, None, Some(rpc::ErrorCode::NotFound)).await;
                    }
                    rpc::Operation::Stat { .. } => {
                        reply(
                            &mut peer,
                            call.id,
                            Some(rpc::Result::Stat {
                                metadata: rpc::Metadata {
                                    kind: rpc::Kind::File,
                                    size: "8".into(),
                                    modified_ms: None,
                                },
                            }),
                            None,
                        )
                        .await;
                    }
                    rpc::Operation::BeginUpload { expected: None, .. } if !finished => {
                        reply(
                            &mut peer,
                            call.id,
                            Some(rpc::Result::Upload { handle: 1 }),
                            None,
                        )
                        .await;
                    }
                    rpc::Operation::Write { handle: 1, .. } if !finished => {
                        let (kind, bytes) = read_frame(&mut peer).await.unwrap();
                        assert_eq!(kind, rpc::DATA);
                        assert_eq!(u32::from_be_bytes(bytes[..4].try_into().unwrap()), call.id);
                        assert_eq!(&bytes[4..], b"new");
                        reply(
                            &mut peer,
                            call.id,
                            Some(rpc::Result::Written { offset: "3".into() }),
                            None,
                        )
                        .await;
                    }
                    rpc::Operation::FinishUpload { handle: 1 } if !finished => {
                        finished = true;
                        collided = true;
                        reply(&mut peer, call.id, None, Some(rpc::ErrorCode::Exists)).await;
                    }
                    rpc::Operation::OpenRead { .. } if collided => {
                        reply(
                            &mut peer,
                            call.id,
                            Some(rpc::Result::File {
                                handle: 7,
                                metadata: rpc::Metadata {
                                    kind: rpc::Kind::File,
                                    size: "8".into(),
                                    modified_ms: None,
                                },
                            }),
                            None,
                        )
                        .await;
                    }
                    rpc::Operation::Close { handle: 7 } => {
                        reply(&mut peer, call.id, Some(rpc::Result::Closed), None).await;
                        let mut eof = [0];
                        assert_eq!(peer.read(&mut eof).await.unwrap(), 0);
                        return;
                    }
                    other => panic!("unexpected operation or automatic replay: {other:?}"),
                }
            }
            panic!("bounded collision fixture did not release its review");
        });
        let endpoint = endpoint(stream, CancellationToken::new()).await;
        let mut manager = SftpTransferManager::from_remote_endpoint(endpoint);
        manager
            .enqueue_batch(vec![SftpTransferRequest::new(
                SftpPath::local(&source),
                SftpPath::remote("/target"),
            )
            .unwrap()])
            .unwrap();
        let collision = loop {
            let event = tokio::time::timeout(Duration::from_secs(3), manager.recv_event())
                .await
                .unwrap()
                .unwrap();
            match event {
                festerm_ssh::SftpTransferEvent::Collision(collision) => break collision,
                festerm_ssh::SftpTransferEvent::ItemFailed { reason, .. } => {
                    panic!("late collision must be reviewed: {reason}")
                }
                _ => {}
            }
        };
        assert_eq!(collision.destination.size, Some(8));
        manager
            .resolve_collision(festerm_ssh::SftpCollisionResolution {
                collision_id: collision.id,
                decision: festerm_ssh::SftpCollisionDecision::Skip,
                scope: festerm_ssh::SftpCollisionScope::ThisItem,
            })
            .unwrap();
        loop {
            let event = tokio::time::timeout(Duration::from_secs(3), manager.recv_event())
                .await
                .unwrap()
                .unwrap();
            if matches!(event, festerm_ssh::SftpTransferEvent::ItemSkipped { .. }) {
                break;
            }
        }
        manager.shutdown().await.unwrap();
        joined(server).await;
        assert_eq!(std::fs::read(source).unwrap(), b"new");
    }

    #[tokio::test]
    async fn files_endpoint_finish_recovery_is_terminal_and_retains_virtual_recovery_path() {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            opened(&mut peer).await;
            let begin = request(&mut peer).await;
            assert!(matches!(
                begin.operation,
                rpc::Operation::BeginUpload { .. }
            ));
            reply(
                &mut peer,
                begin.id,
                Some(rpc::Result::Upload { handle: 1 }),
                None,
            )
            .await;
            let write = request(&mut peer).await;
            assert!(matches!(
                write.operation,
                rpc::Operation::Write { handle: 1, .. }
            ));
            let (kind, bytes) = read_frame(&mut peer).await.unwrap();
            assert_eq!(kind, rpc::DATA);
            assert_eq!(u32::from_be_bytes(bytes[..4].try_into().unwrap()), write.id);
            assert_eq!(&bytes[4..], b"new");
            reply(
                &mut peer,
                write.id,
                Some(rpc::Result::Written { offset: "3".into() }),
                None,
            )
            .await;
            let finish = request(&mut peer).await;
            assert!(matches!(
                finish.operation,
                rpc::Operation::FinishUpload { handle: 1 }
            ));
            write_frame(
                &mut peer,
                rpc::RESPONSE,
                &serde_json::to_vec(&rpc::Response {
                    id: finish.id,
                    result: None,
                    error: Some(rpc::ErrorCode::RecoveryRequired),
                    recovery_path: Some("/folder/.festerm-transfer-owned".into()),
                })
                .unwrap(),
            )
            .await
            .unwrap();
            let mut eof = [0];
            assert_eq!(peer.read(&mut eof).await.unwrap(), 0);
        });
        let mut endpoint = endpoint(stream, CancellationToken::new()).await;
        let mut source = std::io::Cursor::new(b"new");
        let error = endpoint
            .upload("/target", 3, None, &mut source)
            .await
            .unwrap_err();
        let SftpSessionError::PublicationUncertain { path, reason } = error else {
            panic!("recovery must never become a retryable failure");
        };
        assert_eq!(path, "/target");
        assert!(reason.contains("/folder/.festerm-transfer-owned"));
        let details = endpoint.first_failure.details_or("missing".into());
        assert!(
            details.contains("Couldn’t confirm")
                && details.contains("Check the files before trying again")
        );
        assert!(!details.contains("/target") && !details.contains("/folder/"));
        assert!(!endpoint.client.available());
        assert!(endpoint.in_flight_upload.is_none());
        endpoint.cleanup().await.unwrap();
        joined(server).await;
    }

    #[tokio::test]
    async fn files_endpoint_returned_cancel_and_source_error_do_not_leave_unknown_markers() {
        struct FailedSource;
        impl AsyncRead for FailedSource {
            fn poll_read(
                self: Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                _: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
            }
        }
        for cancel_before_begin in [false, true] {
            let (stream, mut peer) = tokio::io::duplex(8192);
            let server = tokio::spawn(async move {
                opened(&mut peer).await;
                if !cancel_before_begin {
                    let begin = request(&mut peer).await;
                    assert!(matches!(
                        begin.operation,
                        rpc::Operation::BeginUpload { .. }
                    ));
                    reply(
                        &mut peer,
                        begin.id,
                        Some(rpc::Result::Upload { handle: 1 }),
                        None,
                    )
                    .await;
                    let close = request(&mut peer).await;
                    assert!(matches!(
                        close.operation,
                        rpc::Operation::Close { handle: 1 }
                    ));
                    reply(&mut peer, close.id, Some(rpc::Result::Closed), None).await;
                    let ping = request(&mut peer).await;
                    assert!(matches!(ping.operation, rpc::Operation::Ping {}));
                    reply(&mut peer, ping.id, Some(rpc::Result::Pong), None).await;
                }
                let mut eof = [0];
                assert_eq!(peer.read(&mut eof).await.unwrap(), 0);
            });
            let lifetime = CancellationToken::new();
            let mut endpoint = endpoint(stream, lifetime.clone()).await;
            if cancel_before_begin {
                lifetime.cancel();
            }
            let result = endpoint.upload("/target", 1, None, &mut FailedSource).await;
            let error = result.unwrap_err();
            let SftpSessionError::RemoteOperationFailed { reason, .. } = error else {
                panic!("returned local refusal must preserve its original failure");
            };
            assert_eq!(
                reason,
                if cancel_before_begin {
                    "rooted file operation cancelled"
                } else {
                    "local transfer I/O failed (BrokenPipe)"
                }
            );
            assert!(endpoint.in_flight_upload.is_none());
            assert_eq!(endpoint.client.available(), !cancel_before_begin);
            if !cancel_before_begin {
                endpoint.keepalive().await.unwrap();
            }
            endpoint.cleanup().await.unwrap();
            endpoint.client.cancel();
            joined(server).await;
        }
    }

    #[tokio::test]
    async fn files_endpoint_transfer_planning_refuses_skipped_names_and_releases_directory() {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            opened(&mut peer).await;
            let open = request(&mut peer).await;
            assert!(matches!(
                open.operation,
                rpc::Operation::OpenDirectory { .. }
            ));
            reply(
                &mut peer,
                open.id,
                Some(rpc::Result::Directory { handle: 2 }),
                None,
            )
            .await;
            let page = request(&mut peer).await;
            assert!(matches!(
                page.operation,
                rpc::Operation::ReadDirectory { handle: 2, .. }
            ));
            reply(
                &mut peer,
                page.id,
                Some(rpc::Result::Entries {
                    entries: Vec::new(),
                    done: true,
                    skipped: 1,
                }),
                None,
            )
            .await;
            let close = request(&mut peer).await;
            assert!(matches!(
                close.operation,
                rpc::Operation::Close { handle: 2 }
            ));
            reply(&mut peer, close.id, Some(rpc::Result::Closed), None).await;
            let mut eof = [0];
            assert_eq!(peer.read(&mut eof).await.unwrap(), 0);
        });
        let mut endpoint = endpoint(stream, CancellationToken::new()).await;
        let handle = endpoint.open_directory("/").await.unwrap();
        let error = match endpoint.read_directory(handle).await {
            Err(error) => error,
            Ok(_) => panic!("incomplete directory must not become a complete plan"),
        };
        assert!(matches!(
            error,
            SftpSessionError::RemoteOperationFailed {
                operation: "plan directory transfer",
                ..
            }
        ));
        endpoint.cleanup().await.unwrap();
        assert!(endpoint.directory.is_none());
        endpoint.client.cancel();
        joined(server).await;
    }

    #[tokio::test]
    async fn files_endpoint_directory_pages_continue_skips_and_refuse_truncated_views() {
        for oversized in [false, true] {
            let (stream, mut peer) = tokio::io::duplex(8192);
            let server = tokio::spawn(async move {
                opened(&mut peer).await;
                let stat = request(&mut peer).await;
                assert!(matches!(stat.operation, rpc::Operation::Stat { .. }));
                reply(
                    &mut peer,
                    stat.id,
                    Some(rpc::Result::Stat {
                        metadata: rpc::Metadata {
                            kind: rpc::Kind::Directory,
                            size: "0".into(),
                            modified_ms: None,
                        },
                    }),
                    None,
                )
                .await;
                let directory = request(&mut peer).await;
                assert!(matches!(
                    directory.operation,
                    rpc::Operation::OpenDirectory { .. }
                ));
                reply(
                    &mut peer,
                    directory.id,
                    Some(rpc::Result::Directory { handle: 2 }),
                    None,
                )
                .await;
                let empty = request(&mut peer).await;
                assert!(matches!(
                    empty.operation,
                    rpc::Operation::ReadDirectory { .. }
                ));
                reply(
                    &mut peer,
                    empty.id,
                    Some(rpc::Result::Entries {
                        entries: Vec::new(),
                        done: false,
                        skipped: 1,
                    }),
                    None,
                )
                .await;
                for page in 0..if oversized { 65 } else { 1 } {
                    let call = request(&mut peer).await;
                    assert!(matches!(
                        call.operation,
                        rpc::Operation::ReadDirectory { .. }
                    ));
                    let count = if oversized { 64 } else { 1 };
                    let entries = (0..count)
                        .map(|index| rpc::Entry {
                            name: format!("owned-{:04}", page * 64 + index),
                            kind: rpc::Kind::File,
                        })
                        .collect();
                    reply(
                        &mut peer,
                        call.id,
                        Some(rpc::Result::Entries {
                            entries,
                            done: !oversized,
                            skipped: 0,
                        }),
                        None,
                    )
                    .await;
                }
                let close = request(&mut peer).await;
                assert!(matches!(
                    close.operation,
                    rpc::Operation::Close { handle: 2 }
                ));
                reply(&mut peer, close.id, Some(rpc::Result::Closed), None).await;
                let mut eof = [0];
                assert_eq!(peer.read(&mut eof).await.unwrap(), 0);
            });
            let mut endpoint = endpoint(stream, CancellationToken::new()).await;
            let result = endpoint.directory_snapshot("/").await;
            if oversized {
                assert!(
                    result.is_err(),
                    "an oversized listing must not return a truncated complete snapshot"
                );
            } else {
                let (snapshot, _, skipped) = result.unwrap();
                assert_eq!(snapshot.entries.len(), 1);
                assert_eq!(skipped, 1);
            }
            assert!(endpoint.directory.is_none());
            assert!(endpoint.client.available());
            endpoint.client.cancel();
            joined(server).await;
        }
    }

    #[tokio::test]
    async fn files_endpoint_preview_refuses_size_before_any_read_and_closes_handle() {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            opened(&mut peer).await;
            let open = request(&mut peer).await;
            assert!(matches!(open.operation, rpc::Operation::OpenRead { .. }));
            reply(
                &mut peer,
                open.id,
                Some(rpc::Result::File {
                    handle: 3,
                    metadata: rpc::Metadata {
                        kind: rpc::Kind::File,
                        size: "5".into(),
                        modified_ms: None,
                    },
                }),
                None,
            )
            .await;
            let close = request(&mut peer).await;
            assert!(matches!(
                close.operation,
                rpc::Operation::Close { handle: 3 }
            ));
            reply(&mut peer, close.id, Some(rpc::Result::Closed), None).await;
            let mut eof = [0];
            assert_eq!(peer.read(&mut eof).await.unwrap(), 0);
        });
        let mut endpoint = endpoint(stream, CancellationToken::new()).await;
        assert!(matches!(
            endpoint.markdown_snapshot("/owned.md", 4).await,
            Err(SftpSessionError::RemoteFileTooLarge { limit_bytes: 4, .. })
        ));
        assert!(endpoint.client.available());
        endpoint.client.cancel();
        joined(server).await;
    }
    fn reviewed_scope() -> rpc::HostScope {
        rpc::HostScope {
            host: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".parse().unwrap(),
            installation: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".parse().unwrap(),
            epoch: "7".into(),
        }
    }
    async fn reviewed_handshake(peer: &mut DuplexStream) {
        let scope = reviewed_scope();
        write_frame(
            peer,
            rpc::HOST_DESCRIPTION,
            &serde_json::to_vec(&rpc::HostDescription {
                version: rpc::HOST_VERSION,
                scope: scope.clone(),
                filesystem: Some(limits()),
            })
            .unwrap(),
        )
        .await
        .unwrap();
        let (kind, bytes) = read_frame(peer).await.unwrap();
        assert_eq!(kind, rpc::OPEN);
        let open: rpc::HostOpen = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(open.version, rpc::HOST_REVIEWED_VERSION);
        assert_eq!(open.scope, scope);
        reply(
            peer,
            0,
            Some(rpc::Result::ReadyV3 {
                capabilities: rpc::OperationCapabilities {
                    version: rpc::HOST_REVIEWED_VERSION,
                    write: true,
                    operations: vec!["delete".into()],
                    chunk_limit: rpc::CHUNK_LIMIT,
                    page_limit: rpc::PAGE_LIMIT,
                    handle_limit: rpc::HANDLE_LIMIT,
                },
            }),
            None,
        )
        .await;
    }
    async fn reviewed_endpoint(stream: DuplexStream) -> FilesEndpoint<DuplexStream> {
        let lifetime = CancellationToken::new();
        let (client, _) = Client::open_host_reviewed(
            stream,
            &reviewed_scope().host,
            &reviewed_scope().installation,
            lifetime.clone(),
        )
        .await
        .unwrap()
        .unwrap();
        FilesEndpoint {
            client,
            lifetime,
            first_failure: FirstFailure::default(),
            channel: FilesChannel::Transfer,
            locally_retired: false,
            next: 1,
            generation: uuid::Uuid::nil(),
            in_flight_upload: None,
            directory: None,
            review: None,
            delete_review: None,
        }
    }
    async fn reviewed_reply(peer: &mut DuplexStream, id: u32, handle: u32, directory: bool) {
        reply(
            peer,
            id,
            Some(rpc::Result::EntryReview {
                handle,
                metadata: rpc::Metadata {
                    kind: if directory {
                        rpc::Kind::Directory
                    } else {
                        rpc::Kind::File
                    },
                    size: if directory { "0" } else { "13" }.into(),
                    modified_ms: Some("1".into()),
                },
            }),
            None,
        )
        .await;
    }

    #[tokio::test]
    async fn files_delete_partial_busy_closes_review_handles_and_preserves_channel() {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            reviewed_handshake(&mut peer).await;
            let first = request(&mut peer).await;
            assert!(
                matches!(first.operation, rpc::Operation::ReviewEntry { ref path, purpose: rpc::ReviewPurpose::Delete } if path == "/one")
            );
            reviewed_reply(&mut peer, first.id, 41, false).await;
            let second = request(&mut peer).await;
            assert!(matches!(
                second.operation,
                rpc::Operation::ReviewEntry { .. }
            ));
            reply(&mut peer, second.id, None, Some(rpc::ErrorCode::Busy)).await;
            let close = request(&mut peer).await;
            assert!(matches!(
                close.operation,
                rpc::Operation::Close { handle: 41 }
            ));
            reply(&mut peer, close.id, Some(rpc::Result::Closed), None).await;
            let ping = request(&mut peer).await;
            assert!(matches!(ping.operation, rpc::Operation::Ping {}));
            reply(&mut peer, ping.id, Some(rpc::Result::Pong), None).await;
        });
        let mut endpoint = reviewed_endpoint(stream).await;
        assert!(endpoint
            .prepare_delete(&["/one".into(), "/two".into()], CancellationToken::new())
            .await
            .is_err());
        assert!(endpoint.delete_review.is_none());
        endpoint.ping().await.unwrap();
        joined(server).await;
    }

    #[tokio::test]
    async fn files_delete_cancelled_preparation_observes_and_closes_last_handle_without_delete() {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let stop = CancellationToken::new();
        let server_stop = stop.clone();
        let server = tokio::spawn(async move {
            reviewed_handshake(&mut peer).await;
            let call = request(&mut peer).await;
            server_stop.cancel();
            reviewed_reply(&mut peer, call.id, 7, false).await;
            let close = request(&mut peer).await;
            assert!(matches!(
                close.operation,
                rpc::Operation::Close { handle: 7 }
            ));
            reply(&mut peer, close.id, Some(rpc::Result::Closed), None).await;
            let ping = request(&mut peer).await;
            assert!(matches!(ping.operation, rpc::Operation::Ping {}));
            reply(&mut peer, ping.id, Some(rpc::Result::Pong), None).await;
        });
        let mut endpoint = reviewed_endpoint(stream).await;
        assert!(endpoint
            .prepare_delete(&["/one".into(), "/never-opened".into()], stop)
            .await
            .is_err());
        assert!(endpoint.delete_review.is_none());
        endpoint.ping().await.unwrap();
        joined(server).await;
    }

    #[tokio::test]
    async fn files_delete_exact_review_batch_reports_confirmed_unknown_and_untouched_without_replay(
    ) {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            reviewed_handshake(&mut peer).await;
            for (index, path) in ["/file", "/empty", "/untouched"].into_iter().enumerate() {
                let call = request(&mut peer).await;
                assert!(
                    matches!(call.operation, rpc::Operation::ReviewEntry { path: ref actual, .. } if actual == path)
                );
                reviewed_reply(&mut peer, call.id, 60 + index as u32, index == 1).await;
            }
            let first = request(&mut peer).await;
            assert!(matches!(
                first.operation,
                rpc::Operation::Delete { review: 60 }
            ));
            reply(&mut peer, first.id, Some(rpc::Result::Deleted), None).await;
            let second = request(&mut peer).await;
            assert!(matches!(
                second.operation,
                rpc::Operation::Delete { review: 61 }
            ));
            // Suppress the already-received second mutation result. No third
            // delete or replay may be attempted after this transport closure.
        });
        let mut endpoint = reviewed_endpoint(stream).await;
        let (id, items) = endpoint
            .prepare_delete(
                &["/file".into(), "/empty".into(), "/untouched".into()],
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(items[0].size, Some(13));
        assert_eq!(items[1].file_type, SftpEntryType::Directory);
        let observer = DeleteObservation::default();
        let outcome = endpoint.commit_delete(id, &observer).await;
        assert_eq!((outcome.confirmed, outcome.total), (1, 3));
        assert!(outcome.failure.unwrap().0);
        assert!(
            observer.interrupted().is_none(),
            "returned unknown clears the owner-drop marker"
        );
        assert!(endpoint.delete_review.is_none());
        assert!(!endpoint.client.available());
        joined(server).await;
    }

    #[tokio::test]
    async fn files_delete_dropped_mutation_retains_only_unknown_observation_and_never_replays() {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let (seen, received) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            reviewed_handshake(&mut peer).await;
            let review = request(&mut peer).await;
            reviewed_reply(&mut peer, review.id, 81, false).await;
            let delete = request(&mut peer).await;
            assert!(matches!(
                delete.operation,
                rpc::Operation::Delete { review: 81 }
            ));
            seen.send(()).unwrap();
            let mut byte = [0];
            assert_eq!(peer.read(&mut byte).await.unwrap(), 0);
        });
        let mut endpoint = reviewed_endpoint(stream).await;
        let (id, _) = endpoint
            .prepare_delete(&["/owned".into()], CancellationToken::new())
            .await
            .unwrap();
        let observer = DeleteObservation::default();
        {
            let mutation = endpoint.commit_delete(id, &observer);
            tokio::pin!(mutation);
            tokio::select! { _ = received => {}, _ = &mut mutation => panic!("withheld result must remain pending") }
        }
        assert!(!endpoint.client.available());
        assert!(matches!(
            observer.interrupted(),
            Some(SftpSessionError::PublicationUncertain { .. })
        ));
        assert!(observer.interrupted().is_none());
        joined(server).await;
    }
    #[tokio::test]
    async fn files_delete_returned_refusal_and_success_clear_owner_marker_and_keep_channel_live() {
        for refuse in [false, true] {
            let (stream, mut peer) = tokio::io::duplex(8192);
            let server = tokio::spawn(async move {
                reviewed_handshake(&mut peer).await;
                for index in 0..2 {
                    let call = request(&mut peer).await;
                    reviewed_reply(&mut peer, call.id, 90 + index, index == 1).await;
                }
                let first = request(&mut peer).await;
                assert!(matches!(
                    first.operation,
                    rpc::Operation::Delete { review: 90 }
                ));
                if refuse {
                    reply(&mut peer, first.id, None, Some(rpc::ErrorCode::Conflict)).await;
                } else {
                    reply(&mut peer, first.id, Some(rpc::Result::Deleted), None).await;
                }
                let second = request(&mut peer).await;
                if refuse {
                    assert!(matches!(
                        second.operation,
                        rpc::Operation::Close { handle: 91 }
                    ));
                    reply(&mut peer, second.id, Some(rpc::Result::Closed), None).await;
                } else {
                    assert!(matches!(
                        second.operation,
                        rpc::Operation::Delete { review: 91 }
                    ));
                    reply(&mut peer, second.id, Some(rpc::Result::Deleted), None).await;
                }
                let ping = request(&mut peer).await;
                assert!(matches!(ping.operation, rpc::Operation::Ping {}));
                reply(&mut peer, ping.id, Some(rpc::Result::Pong), None).await;
            });
            let mut endpoint = reviewed_endpoint(stream).await;
            let (id, _) = endpoint
                .prepare_delete(&["/file".into(), "/empty".into()], CancellationToken::new())
                .await
                .unwrap();
            let observer = DeleteObservation::default();
            let outcome = endpoint.commit_delete(id, &observer).await;
            assert_eq!(outcome.total, 2);
            if refuse {
                assert_eq!(outcome.confirmed, 0);
                assert!(
                    !outcome.failure.unwrap().0,
                    "fully received Conflict is a known refusal"
                );
            } else {
                assert_eq!(outcome.confirmed, 2);
                assert!(outcome.failure.is_none());
            }
            assert!(observer.interrupted().is_none());
            assert!(endpoint.delete_review.is_none());
            endpoint.ping().await.unwrap();
            joined(server).await;
        }
    }
    #[tokio::test]
    async fn files_delete_recovery_required_preserves_virtual_path_and_is_terminal_unknown() {
        let (stream, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            reviewed_handshake(&mut peer).await;
            let call = request(&mut peer).await;
            reviewed_reply(&mut peer, call.id, 22, false).await;
            let delete = request(&mut peer).await;
            assert!(matches!(
                delete.operation,
                rpc::Operation::Delete { review: 22 }
            ));
            write_frame(
                &mut peer,
                rpc::RESPONSE,
                &serde_json::to_vec(&rpc::Response {
                    id: delete.id,
                    result: None,
                    error: Some(rpc::ErrorCode::RecoveryRequired),
                    recovery_path: Some("/folder/.festerm-transfer-delete-owned".into()),
                })
                .unwrap(),
            )
            .await
            .unwrap();
            let mut byte = [0];
            assert_eq!(peer.read(&mut byte).await.unwrap(), 0);
        });
        let mut endpoint = reviewed_endpoint(stream).await;
        let (id, _) = endpoint
            .prepare_delete(&["/target".into()], CancellationToken::new())
            .await
            .unwrap();
        let observer = DeleteObservation::default();
        let outcome = endpoint.commit_delete(id, &observer).await;
        assert_eq!((outcome.confirmed, outcome.total), (0, 1));
        let (unknown, reason) = outcome.failure.unwrap();
        assert!(unknown);
        assert!(reason.contains("/folder/.festerm-transfer-delete-owned"));
        assert!(!endpoint.client.available());
        assert!(observer.interrupted().is_none());
        joined(server).await;
    }
}
