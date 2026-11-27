//! Narrow composition boundary for a rooted remote filesystem. The scheduler,
//! planning reservations and local staging remain owned by the existing queue.
use super::*;
use tokio::io::{AsyncRead, AsyncWrite};

pub type RemoteTransferFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, SftpSessionError>> + Send + 'a>>;

/// Endpoint-issued, connection-scoped identity for the object reviewed before
/// replacement. This is never a path, metadata approximation, or persisted ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteReplacementReview(pub u64);

/// One bounded directory page. The endpoint must not collect the full listing.
pub struct RemoteDirectoryPage {
    pub entries: Vec<SftpDirectoryItem>,
    pub done: bool,
}

/// A worker-owned remote endpoint; it confers no authority by itself. Upload
/// owns atomic publication, while downloads write only to queue-owned staging.
/// Every operation must enforce the endpoint's current account/lifetime scope.
pub trait RemoteTransferEndpoint: Send + 'static {
    fn check_current(&mut self) -> Result<(), SftpSessionError>;
    fn retirement(&self) -> Pin<Box<dyn Future<Output = ()> + Send>>;
    /// Opt-in idle liveness only; ordinary SSH endpoints remain unchanged.
    fn idle_keepalive_interval(&self) -> Option<std::time::Duration> {
        None
    }
    /// Completes one exchange before the worker admits another command. A
    /// terminal failure must retire this endpoint's own connection lifetime.
    fn keepalive(&mut self) -> RemoteTransferFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    /// Close all retained directory/review handles on worker shutdown or a
    /// dropped operation. A failed close must retire the endpoint locally.
    fn cleanup(&mut self) -> RemoteTransferFuture<'_, ()>;
    fn metadata<'a>(
        &'a mut self,
        path: &'a str,
    ) -> RemoteTransferFuture<'a, Option<SftpPathMetadata>>;
    fn open_directory<'a>(&'a mut self, path: &'a str) -> RemoteTransferFuture<'a, u64>;
    fn read_directory(&mut self, handle: u64) -> RemoteTransferFuture<'_, RemoteDirectoryPage>;
    fn close_directory(&mut self, handle: u64) -> RemoteTransferFuture<'_, ()>;
    fn mkdir<'a>(&'a mut self, path: &'a str) -> RemoteTransferFuture<'a, ()>;
    /// Called only after reserving the queue's sole remote review admission.
    fn review_replacement<'a>(
        &'a mut self,
        path: &'a str,
    ) -> RemoteTransferFuture<'a, (RemoteReplacementReview, SftpPathMetadata)>;
    fn release_replacement(
        &mut self,
        review: RemoteReplacementReview,
    ) -> RemoteTransferFuture<'_, ()>;
    fn upload<'a>(
        &'a mut self,
        path: &'a str,
        size: u64,
        expected: Option<RemoteReplacementReview>,
        source: &'a mut (dyn AsyncRead + Unpin + Send),
    ) -> RemoteTransferFuture<'a, u64>;
    fn download<'a>(
        &'a mut self,
        path: &'a str,
        sink: &'a mut (dyn AsyncWrite + Unpin + Send),
    ) -> RemoteTransferFuture<'a, u64>;
}

/// A caller-admitted regular local source. The reader must retain its object,
/// fence current authority/path/identity on every read, and never reopen by name.
pub struct OpenedLocalUpload {
    pub reader: Pin<Box<dyn AsyncRead + Send>>,
    pub size: u64,
}

/// Optional application-owned source admission for rooted app storage. It
/// grants no remote authority and supports only already-retained regular files.
/// Existing ordinary SFTP constructors keep their original path semantics.
pub trait LocalUploadSources: Send + Sync + 'static {
    fn metadata(&self, path: &Path) -> Result<SftpPathMetadata, SftpSessionError>;
    fn open(&self, path: &Path) -> Result<OpenedLocalUpload, SftpSessionError>;
}

/// Capability-issued identity for one regular local target reviewed before
/// replacement. It is not a path, wire handle, or persisted authorization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalReplacementReview(pub u64);

/// One retained local staging file. Implementations fence current authority and
/// held root/parent/file identities on writes, publication and cleanup. Creation
/// is synchronous so its ownership is recorded before the first async handoff.
/// Publication must be create-only without a review, or atomically replace only
/// its exact reviewed target; it must never unlink the destination first.
pub trait LocalDownloadFile: AsyncWrite + Unpin + Send {
    fn publish(&mut self) -> Result<(), SftpSessionError>;
    fn cleanup(&mut self) -> Result<(), SftpSessionError>;
}

/// Opt-in retained local storage for regular-file downloads. Every method must
/// enforce the admitted root and current application authority without reopening
/// by an unchecked pathname. A failed create retains/cleans its own partial
/// ownership; returned stages remain charged to the queue until cleanup.
pub trait LocalDownloadDestinations: Send + 'static {
    fn validate_source(&self, source: &SftpPathMetadata) -> Result<(), SftpSessionError>;
    fn transfer_timeout(&self) -> std::time::Duration;
    fn metadata(&self, path: &Path) -> Result<Option<SftpPathMetadata>, SftpSessionError>;
    fn review_replacement(
        &mut self,
        path: &Path,
    ) -> Result<(LocalReplacementReview, SftpPathMetadata), SftpSessionError>;
    fn release_replacement(
        &mut self,
        review: LocalReplacementReview,
    ) -> Result<(), SftpSessionError>;
    fn create(
        &mut self,
        path: &Path,
        expected: Option<LocalReplacementReview>,
    ) -> Result<Box<dyn LocalDownloadFile>, SftpSessionError>;
}

pub(super) struct EndpointBackend<R> {
    pub(super) remote: R,
    pub(super) partial: Option<crate::sftp::PartialFile>,
    pub(super) local_upload_sources: Option<Arc<dyn LocalUploadSources>>,
    pub(super) local_downloads: Option<Box<dyn LocalDownloadDestinations>>,
    pub(super) local_download_file: Option<Box<dyn LocalDownloadFile>>,
}

impl<R: RemoteTransferEndpoint> TransferBackend for EndpointBackend<R> {
    fn atomic_publication(&self, destination: &SftpPath) -> bool {
        destination.location() == SftpLocation::Remote || self.local_downloads.is_some()
    }
    fn validate_download_source(
        &self,
        source: &SftpPathMetadata,
        destination: &SftpPath,
    ) -> Result<(), SftpSessionError> {
        if source.path.location() == SftpLocation::Remote
            && destination.location() == SftpLocation::Local
        {
            if let Some(downloads) = &self.local_downloads {
                downloads.validate_source(source)?;
            }
        }
        Ok(())
    }
    fn retirement(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        self.remote.retirement()
    }
    fn idle_keepalive_interval(&self) -> Option<std::time::Duration> {
        self.remote.idle_keepalive_interval()
    }
    fn keepalive(&mut self) -> BackendFuture<'_, ()> {
        self.remote.keepalive()
    }
    fn review_replacement<'a>(
        &'a mut self,
        path: &'a SftpPath,
    ) -> BackendFuture<'a, Option<(ReplacementReview, SftpPathMetadata)>> {
        Box::pin(async move {
            self.remote.check_current()?;
            match path {
                SftpPath::Remote(path) => self
                    .remote
                    .review_replacement(path)
                    .await
                    .map(|(review, metadata)| Some((ReplacementReview::Remote(review), metadata))),
                SftpPath::Local(path) => match &mut self.local_downloads {
                    Some(downloads) => {
                        downloads
                            .review_replacement(path)
                            .map(|(review, metadata)| {
                                Some((ReplacementReview::Local(review), metadata))
                            })
                    }
                    None => Ok(None),
                },
            }
        })
    }
    fn release_replacement(&mut self, review: ReplacementReview) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            match review {
                ReplacementReview::Remote(review) => self.remote.release_replacement(review).await,
                ReplacementReview::Local(review) => self
                    .local_downloads
                    .as_mut()
                    .ok_or_else(|| {
                        endpoint_error(
                            "release replacement",
                            "local",
                            "local capability unavailable",
                        )
                    })?
                    .release_replacement(review),
            }
        })
    }
    fn protect_partial_commit<'a>(
        &'a mut self,
        temporary: &'a SftpPath,
        destination: &'a SftpPath,
    ) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            crate::sftp::protect_transfer_partial(&mut self.partial, temporary, destination);
            Ok(())
        })
    }
    fn cleanup_interrupted_copy(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            let retained = self
                .local_download_file
                .take()
                .map(|mut file| file.cleanup())
                .unwrap_or(Ok(()));
            let local = crate::sftp::cleanup_transfer_partial(&mut self.partial).await;
            let remote = self.remote.cleanup().await;
            let mut failure = None;
            for result in [retained, local, remote] {
                if let Err(cleanup_error) = result {
                    failure = Some(match failure {
                        Some(operation_error) => {
                            SftpSessionError::TransferFailedWithIncompleteCleanup {
                                operation_error: Box::new(operation_error),
                                cleanup_error: Box::new(cleanup_error),
                            }
                        }
                        None => cleanup_error,
                    });
                }
            }
            failure.map_or(Ok(()), Err)
        })
    }
    fn metadata<'a>(
        &'a mut self,
        path: &'a SftpPath,
    ) -> BackendFuture<'a, Option<SftpPathMetadata>> {
        Box::pin(async move {
            self.remote.check_current()?;
            match path {
                SftpPath::Local(path) => {
                    if let Some(downloads) = &self.local_downloads {
                        downloads.metadata(path)
                    } else {
                        match &self.local_upload_sources {
                            Some(sources) => sources.metadata(path).map(Some),
                            None => read_local_path_metadata(path).await,
                        }
                    }
                }
                SftpPath::Remote(path) => self.remote.metadata(path).await,
            }
        })
    }
    fn read_directory<'a>(
        &'a mut self,
        path: &'a SftpPath,
        budget: &'a SharedPlanningBudget,
    ) -> DirectoryFuture<'a> {
        Box::pin(async move {
            self.remote.check_current()?;
            let SftpPath::Remote(path) = path else {
                let SftpPath::Local(path) = path else {
                    unreachable!()
                };
                if self.local_upload_sources.is_some() || self.local_downloads.is_some() {
                    return Err(endpoint_error(
                        "plan upload",
                        &path.display().to_string(),
                        "Only admitted regular files can be uploaded",
                    )
                    .into());
                }
                return read_local_directory_for_planning(path, budget).await;
            };
            let handle = self.remote.open_directory(path).await?;
            let result = async {
                let mut entries = PlanningQueue::new(budget);
                for _ in 0..1024 {
                    let page = self.remote.read_directory(handle).await?;
                    if page.entries.len() > 64 {
                        return Err(endpoint_error(
                            "read directory",
                            path,
                            "remote page exceeded 64 entries",
                        )
                        .into());
                    }
                    for entry in page.entries {
                        crate::sftp::validate_sftp_child_name(&entry.name)?;
                        if entry.path != SftpPath::remote(join_remote_path(path, &entry.name)) {
                            return Err(endpoint_error(
                                "read directory",
                                path,
                                "remote child path does not match its parent",
                            )
                            .into());
                        }
                        let reservation = budget
                            .entry(entry.name.capacity(), path_memory_proxy_bytes(&entry.path))?;
                        entries.push_back(Budgeted {
                            value: entry,
                            reservation,
                        })?;
                    }
                    if page.done {
                        return Ok(entries);
                    }
                }
                Err(endpoint_error(
                    "read directory",
                    path,
                    "directory exceeded its bounded page count",
                )
                .into())
            }
            .await;
            let closed = self.remote.close_directory(handle).await;
            match (result, closed) {
                (Ok(entries), Ok(())) => Ok(entries),
                (Err(error), Ok(())) => Err(error),
                (operation, Err(close_error)) => Err(TransferWorkError::DirectoryCloseFailed {
                    operation_error: operation.err().map(Box::new),
                    close_error,
                }),
            }
        })
    }
    fn create_directory<'a>(&'a mut self, path: &'a SftpPath) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            self.remote.check_current()?;
            match path {
                SftpPath::Local(path) if self.local_downloads.is_some() => Err(endpoint_error(
                    "create directory",
                    &path.display().to_string(),
                    "local download capability admits regular files only",
                )),
                SftpPath::Local(path) => fs::create_dir(path)
                    .await
                    .map_err(|error| local_error("create directory", path, error)),
                SftpPath::Remote(path) => self.remote.mkdir(path).await,
            }
        })
    }
    fn remove_file<'a>(&'a mut self, path: &'a SftpPath) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            self.remote.check_current()?;
            let SftpPath::Local(local) = path else {
                return Err(endpoint_error(
                    "remove file",
                    &path.display(),
                    "rooted v1 does not grant delete",
                ));
            };
            if self.local_downloads.is_some() {
                return Err(endpoint_error(
                    "remove file",
                    &path.display(),
                    "retained local publication never removes by path",
                ));
            }
            crate::sftp::check_transfer_partial_path(&self.partial, path).await?;
            fs::remove_file(local)
                .await
                .map_err(|error| local_error("remove file", local, error))?;
            crate::sftp::forget_transfer_partial(&mut self.partial, path);
            Ok(())
        })
    }
    fn rename<'a>(
        &'a mut self,
        source: &'a SftpPath,
        destination: &'a SftpPath,
    ) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            self.remote.check_current()?;
            let (SftpPath::Local(from), SftpPath::Local(to)) = (source, destination) else {
                return Err(endpoint_error(
                    "rename file",
                    &destination.display(),
                    "rooted v1 does not grant rename",
                ));
            };
            if self.local_downloads.is_some() {
                return Err(endpoint_error(
                    "rename file",
                    &destination.display(),
                    "retained local publication never renames by path",
                ));
            }
            crate::sftp::check_transfer_partial_path(&self.partial, source).await?;
            fs::rename(from, to)
                .await
                .map_err(|error| local_error("rename file", from, error))?;
            crate::sftp::forget_transfer_partial(&mut self.partial, source);
            Ok(())
        })
    }
    fn copy_file<'a>(
        &'a mut self,
        source: &'a SftpPath,
        destination: &'a SftpPath,
        expected: Option<ReplacementReview>,
        on_progress: &'a mut (dyn FnMut(u64) -> Result<(), CopyInterrupted> + Send),
    ) -> CopyFuture<'a> {
        Box::pin(async move {
            self.remote
                .check_current()
                .map_err(CopyFileError::Operation)?;
            match (source, destination) {
                (SftpPath::Local(source), SftpPath::Remote(destination)) => {
                    let expected = match expected {
                        Some(ReplacementReview::Remote(review)) => Some(review),
                        None => None,
                        Some(ReplacementReview::Local(_)) => {
                            return Err(CopyFileError::Operation(endpoint_error(
                                "upload",
                                destination,
                                "local review cannot authorize remote publication",
                            )))
                        }
                    };
                    let (file, size) = if let Some(sources) = &self.local_upload_sources {
                        let opened = sources.open(source).map_err(CopyFileError::Operation)?;
                        (opened.reader, opened.size)
                    } else {
                        let file = fs::File::open(source).await.map_err(|error| {
                            CopyFileError::Operation(local_error("open source file", source, error))
                        })?;
                        let size = file
                            .metadata()
                            .await
                            .map_err(|error| {
                                CopyFileError::Operation(local_error(
                                    "read source metadata",
                                    source,
                                    error,
                                ))
                            })?
                            .len();
                        (Box::pin(file) as Pin<Box<dyn AsyncRead + Send>>, size)
                    };
                    let mut reader = ProgressIo {
                        inner: file,
                        total: 0,
                        progress: on_progress,
                        interrupted: false,
                    };
                    let result = self
                        .remote
                        .upload(destination, size, expected, &mut reader)
                        .await;
                    copy_result(result, reader.interrupted)
                }
                (SftpPath::Remote(source), SftpPath::Local(destination)) => {
                    if let Some(downloads) = &mut self.local_downloads {
                        let expected = match expected {
                            Some(ReplacementReview::Local(review)) => Some(review),
                            None => None,
                            Some(ReplacementReview::Remote(_)) => {
                                return Err(CopyFileError::Operation(endpoint_error(
                                    "download",
                                    source,
                                    "remote review cannot authorize local publication",
                                )))
                            }
                        };
                        if self.local_download_file.is_some() {
                            return Err(CopyFileError::Operation(endpoint_error(
                                "download",
                                source,
                                "previous retained staging still requires cleanup",
                            )));
                        }
                        let timeout = downloads.transfer_timeout();
                        // Synchronous ownership is stored before any await.
                        self.local_download_file = Some(
                            downloads
                                .create(destination, expected)
                                .map_err(CopyFileError::Operation)?,
                        );
                        let (result, interrupted) = {
                            let file = self
                                .local_download_file
                                .as_mut()
                                .expect("retained local download");
                            let mut writer = ProgressIo {
                                inner: file,
                                total: 0,
                                progress: &mut *on_progress,
                                interrupted: false,
                            };
                            let result = tokio::time::timeout(timeout, async {
                                let bytes = self.remote.download(source, &mut writer).await?;
                                tokio::io::AsyncWriteExt::flush(&mut writer).await.map_err(
                                    |error| local_error("flush download", destination, error),
                                )?;
                                Ok(bytes)
                            })
                            .await
                            .unwrap_or_else(|_| {
                                Err(endpoint_error(
                                    "download",
                                    source,
                                    "download operation timed out",
                                ))
                            });
                            (result, writer.interrupted)
                        };
                        let bytes = copy_result(result, interrupted)?;
                        self.remote
                            .check_current()
                            .map_err(CopyFileError::Operation)?;
                        // Process cancellation that arrived with the final byte before publication.
                        on_progress(bytes).map_err(|_| CopyFileError::Cancelled)?;
                        let publication = self
                            .local_download_file
                            .as_mut()
                            .expect("retained local download")
                            .publish();
                        let mut file = self
                            .local_download_file
                            .take()
                            .expect("retained local download");
                        let cleanup = file.cleanup();
                        match (publication, cleanup) {
                            (Ok(()), Ok(())) => {}
                            (Err(error), Ok(())) | (Ok(()), Err(error)) => {
                                return Err(CopyFileError::Operation(error))
                            }
                            (Err(operation_error), Err(cleanup_error)) => {
                                return Err(CopyFileError::Operation(
                                    SftpSessionError::TransferFailedWithIncompleteCleanup {
                                        operation_error: Box::new(operation_error),
                                        cleanup_error: Box::new(cleanup_error),
                                    },
                                ))
                            }
                        }
                        return Ok(bytes);
                    }
                    crate::sftp::begin_transfer_partial(
                        &mut self.partial,
                        SftpPath::local(destination),
                    )
                    .map_err(CopyFileError::Operation)?;
                    let file =
                        crate::sftp::create_transfer_download_file(&mut self.partial, destination)
                            .await
                            .map_err(CopyFileError::Operation)?;
                    let mut writer = ProgressIo {
                        inner: file,
                        total: 0,
                        progress: on_progress,
                        interrupted: false,
                    };
                    let result = self.remote.download(source, &mut writer).await;
                    copy_result(result, writer.interrupted)
                }
                _ => Err(CopyFileError::Operation(endpoint_error(
                    "copy file",
                    &destination.display(),
                    "copy requires one local and one remote path",
                ))),
            }
        })
    }
}

pub(super) fn endpoint_error(
    operation: &'static str,
    path: &str,
    reason: &str,
) -> SftpSessionError {
    SftpSessionError::RemoteOperationFailed {
        operation,
        path: path.into(),
        reason: reason.into(),
    }
}
fn copy_result(
    result: Result<u64, SftpSessionError>,
    interrupted: bool,
) -> Result<u64, CopyFileError> {
    match result {
        // An uncertain publication must retain its original classification.
        Err(
            error @ (SftpSessionError::PublicationUncertain { .. }
            | SftpSessionError::TransferFailedWithIncompleteCleanup { .. }),
        ) => Err(CopyFileError::Operation(error)),
        Err(_) if interrupted => Err(CopyFileError::Cancelled),
        other => other.map_err(CopyFileError::Operation),
    }
}

struct ProgressIo<'a, T> {
    inner: T,
    total: u64,
    progress: &'a mut (dyn FnMut(u64) -> Result<(), CopyInterrupted> + Send),
    interrupted: bool,
}
impl<T: AsyncRead + Unpin> AsyncRead for ProgressIo<'_, T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let total = self.total;
        if (self.progress)(total).is_err() {
            self.interrupted = true;
            return std::task::Poll::Ready(Err(std::io::ErrorKind::Interrupted.into()));
        }
        let before = buffer.filled().len();
        match Pin::new(&mut self.inner).poll_read(cx, buffer) {
            std::task::Poll::Ready(Ok(())) => {
                self.total += (buffer.filled().len() - before) as u64;
                std::task::Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}
impl<T: AsyncWrite + Unpin> AsyncWrite for ProgressIo<'_, T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buffer: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let total = self.total;
        if (self.progress)(total).is_err() {
            self.interrupted = true;
            return std::task::Poll::Ready(Err(std::io::ErrorKind::Interrupted.into()));
        }
        match Pin::new(&mut self.inner).poll_write(cx, buffer) {
            std::task::Poll::Ready(Ok(written)) => {
                self.total += written as u64;
                std::task::Poll::Ready(Ok(written))
            }
            other => other,
        }
    }
    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let total = self.total;
        if (self.progress)(total).is_err() {
            self.interrupted = true;
            return std::task::Poll::Ready(Err(std::io::ErrorKind::Interrupted.into()));
        }
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[test]
    fn interrupted_copy_preserves_failed_handle_cleanup() {
        let error = SftpSessionError::TransferFailedWithIncompleteCleanup {
            operation_error: Box::new(endpoint_error("upload", "/owned", "local interrupted")),
            cleanup_error: Box::new(endpoint_error(
                "close transfer handle",
                "/owned",
                "deadline",
            )),
        };
        assert!(matches!(
            copy_result(Err(error), true),
            Err(CopyFileError::Operation(
                SftpSessionError::TransferFailedWithIncompleteCleanup { .. }
            ))
        ));
    }
    #[cfg(unix)]
    use tokio::io::AsyncWriteExt;

    #[cfg(unix)]
    #[test]
    fn unix_download_endpoint_refuses_changed_ancestor_before_remove_or_rename() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                for remove_destination in [true, false] {
                    let marker = source(if remove_destination {
                        "unix-remove-fence"
                    } else {
                        "unix-rename-fence"
                    });
                    let root = marker.parent().unwrap();
                    let selected = root.join("selected");
                    let moved = root.join("moved");
                    std::fs::create_dir(&selected).unwrap();
                    let temporary = selected.join("owned.part");
                    let destination = selected.join("final");
                    std::fs::write(&destination, b"reviewed destination").unwrap();
                    let mut backend = EndpointBackend {
                        remote: ScriptedRemote(Arc::new(Mutex::new(RemoteState::default()))),
                        partial: None,
                        local_upload_sources: None,
                        local_downloads: None,
                        local_download_file: None,
                    };
                    crate::sftp::begin_transfer_partial(
                        &mut backend.partial,
                        SftpPath::local(&temporary),
                    )
                    .unwrap();
                    let mut writer = crate::sftp::create_transfer_download_file(
                        &mut backend.partial,
                        &temporary,
                    )
                    .await
                    .unwrap();
                    writer.write_all(b"owned output").await.unwrap();
                    writer.flush().await.unwrap();
                    drop(writer);
                    backend
                        .protect_partial_commit(
                            &SftpPath::local(&temporary),
                            &SftpPath::local(&destination),
                        )
                        .await
                        .unwrap();
                    std::fs::rename(&selected, &moved).unwrap();
                    std::fs::create_dir(&selected).unwrap();
                    std::fs::write(&temporary, b"foreign temporary").unwrap();
                    std::fs::write(&destination, b"foreign destination").unwrap();
                    let result = if remove_destination {
                        backend.remove_file(&SftpPath::local(&destination)).await
                    } else {
                        backend
                            .rename(&SftpPath::local(&temporary), &SftpPath::local(&destination))
                            .await
                    };
                    assert!(matches!(
                        result,
                        Err(SftpSessionError::PartialFileOwnershipUnconfirmed { .. })
                    ));
                    assert_eq!(std::fs::read(&temporary).unwrap(), b"foreign temporary");
                    assert_eq!(std::fs::read(&destination).unwrap(), b"foreign destination");
                    assert_eq!(
                        std::fs::read(moved.join("owned.part")).unwrap(),
                        b"owned output"
                    );
                    assert_eq!(
                        std::fs::read(moved.join("final")).unwrap(),
                        b"reviewed destination"
                    );
                    assert!(matches!(
                        backend.cleanup_interrupted_copy().await,
                        Err(SftpSessionError::TransferCommitInterrupted { .. })
                    ));
                    std::fs::remove_dir_all(root).unwrap();
                }
            });
    }

    #[derive(Default)]
    struct RemoteState {
        files: HashMap<String, Vec<u8>>,
        review: Option<(String, Vec<u8>)>,
        events: Vec<&'static str>,
        uncertain: bool,
    }
    struct ScriptedRemote(Arc<Mutex<RemoteState>>);
    impl RemoteTransferEndpoint for ScriptedRemote {
        fn check_current(&mut self) -> Result<(), SftpSessionError> {
            Ok(())
        }
        fn retirement(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            Box::pin(std::future::pending())
        }
        fn cleanup(&mut self) -> RemoteTransferFuture<'_, ()> {
            Box::pin(async move {
                self.0.lock().unwrap().review = None;
                Ok(())
            })
        }
        fn metadata<'a>(
            &'a mut self,
            path: &'a str,
        ) -> RemoteTransferFuture<'a, Option<SftpPathMetadata>> {
            Box::pin(async move {
                let state = self.0.lock().unwrap();
                Ok(if path == "/" {
                    Some(info(path, SftpEntryType::Directory, 0))
                } else {
                    state
                        .files
                        .get(path)
                        .map(|bytes| info(path, SftpEntryType::File, bytes.len() as u64))
                })
            })
        }
        fn open_directory<'a>(&'a mut self, _: &'a str) -> RemoteTransferFuture<'a, u64> {
            Box::pin(async { panic!("no directory enumeration in file fixture") })
        }
        fn read_directory(&mut self, _: u64) -> RemoteTransferFuture<'_, RemoteDirectoryPage> {
            Box::pin(async { panic!("no directory enumeration in file fixture") })
        }
        fn close_directory(&mut self, _: u64) -> RemoteTransferFuture<'_, ()> {
            Box::pin(async { panic!("no directory enumeration in file fixture") })
        }
        fn mkdir<'a>(&'a mut self, _: &'a str) -> RemoteTransferFuture<'a, ()> {
            Box::pin(async { panic!("no mkdir in file fixture") })
        }
        fn review_replacement<'a>(
            &'a mut self,
            path: &'a str,
        ) -> RemoteTransferFuture<'a, (RemoteReplacementReview, SftpPathMetadata)> {
            Box::pin(async move {
                let mut state = self.0.lock().unwrap();
                assert!(
                    state.review.is_none(),
                    "review admission must precede the second handle"
                );
                let bytes = state.files[path].clone();
                let metadata = info(path, SftpEntryType::File, bytes.len() as u64);
                state.review = Some((path.into(), bytes));
                state.events.push("review");
                Ok((RemoteReplacementReview(7), metadata))
            })
        }
        fn release_replacement(
            &mut self,
            review: RemoteReplacementReview,
        ) -> RemoteTransferFuture<'_, ()> {
            Box::pin(async move {
                assert_eq!(review, RemoteReplacementReview(7));
                let mut state = self.0.lock().unwrap();
                assert!(state.review.take().is_some());
                state.events.push("release");
                Ok(())
            })
        }
        fn upload<'a>(
            &'a mut self,
            path: &'a str,
            size: u64,
            expected: Option<RemoteReplacementReview>,
            source: &'a mut (dyn AsyncRead + Unpin + Send),
        ) -> RemoteTransferFuture<'a, u64> {
            Box::pin(async move {
                let mut bytes = Vec::new();
                source.read_to_end(&mut bytes).await.unwrap();
                assert_eq!(size, bytes.len() as u64);
                let mut state = self.0.lock().unwrap();
                state.events.push("upload");
                if let Some(review) = expected {
                    assert_eq!(review, RemoteReplacementReview(7));
                    let (reviewed, original) = state
                        .review
                        .as_ref()
                        .expect("exact review retained through upload");
                    assert_eq!(reviewed, path);
                    if state.files.get(path) != Some(original) {
                        return Err(endpoint_error("publish", path, "reviewed object changed"));
                    }
                } else if state.files.contains_key(path) {
                    return Err(SftpSessionError::DestinationExists { path: path.into() });
                }
                state.files.insert(path.into(), bytes);
                if state.uncertain {
                    Err(SftpSessionError::PublicationUncertain {
                        path: path.into(),
                        reason: "scripted lost finish acknowledgement".into(),
                    })
                } else {
                    Ok(size)
                }
            })
        }
        fn download<'a>(
            &'a mut self,
            path: &'a str,
            sink: &'a mut (dyn AsyncWrite + Unpin + Send),
        ) -> RemoteTransferFuture<'a, u64> {
            Box::pin(async move {
                let bytes = {
                    let mut state = self.0.lock().unwrap();
                    state.events.push("download");
                    state.files[path].clone()
                };
                tokio::io::AsyncWriteExt::write_all(sink, &bytes)
                    .await
                    .map_err(|error| endpoint_error("download", path, &error.to_string()))?;
                Ok(bytes.len() as u64)
            })
        }
    }
    fn info(path: &str, file_type: SftpEntryType, size: u64) -> SftpPathMetadata {
        SftpPathMetadata {
            path: SftpPath::remote(path),
            file_type,
            size: Some(size),
            modified_at: None,
            permissions: Some(0o700),
        }
    }
    async fn next_event(manager: &mut SftpTransferManager) -> SftpTransferEvent {
        tokio::time::timeout(std::time::Duration::from_secs(2), manager.recv_event())
            .await
            .unwrap()
            .unwrap()
    }
    fn source(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "festerm-endpoint-{}-{name}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("source");
        std::fs::write(&path, b"new contents").unwrap();
        path
    }
    fn resolve(
        manager: &SftpTransferManager,
        collision: &SftpCollision,
        decision: SftpCollisionDecision,
    ) {
        manager
            .resolve_collision(SftpCollisionResolution {
                collision_id: collision.id,
                decision,
                scope: SftpCollisionScope::ThisItem,
            })
            .unwrap();
    }

    #[test]
    fn rooted_queue_parks_second_replacement_until_first_review_releases() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let source = source("review-admission");
                let state = Arc::new(Mutex::new(RemoteState {
                    files: HashMap::from([
                        ("/a".into(), b"old a".to_vec()),
                        ("/b".into(), b"old b".to_vec()),
                    ]),
                    ..Default::default()
                }));
                let mut manager =
                    SftpTransferManager::from_remote_endpoint(ScriptedRemote(state.clone()));
                manager
                    .enqueue_batch(
                        ["/a", "/b"]
                            .into_iter()
                            .map(|path| {
                                SftpTransferRequest::new(
                                    SftpPath::local(&source),
                                    SftpPath::remote(path),
                                )
                                .unwrap()
                            })
                            .collect(),
                    )
                    .unwrap();
                let first = loop {
                    if let SftpTransferEvent::Collision(value) = next_event(&mut manager).await {
                        break value;
                    }
                };
                tokio::task::yield_now().await;
                assert_eq!(state.lock().unwrap().events, ["review"]);
                resolve(&manager, &first, SftpCollisionDecision::Skip);
                let second = loop {
                    if let SftpTransferEvent::Collision(value) = next_event(&mut manager).await {
                        break value;
                    }
                };
                assert_ne!(first.transfer_id, second.transfer_id);
                assert_eq!(
                    state.lock().unwrap().events,
                    ["review", "release", "review"]
                );
                manager.cancel_all_transfers().unwrap();
                loop {
                    if matches!(
                        next_event(&mut manager).await,
                        SftpTransferEvent::BatchFinished { .. }
                    ) {
                        break;
                    }
                }
                assert_eq!(
                    state.lock().unwrap().events,
                    ["review", "release", "review", "release"]
                );
                manager.shutdown().await.unwrap();
                assert_eq!(state.lock().unwrap().files["/a"], b"old a");
                assert_eq!(state.lock().unwrap().files["/b"], b"old b");
            });
    }

    #[test]
    fn rooted_queue_replacement_retains_review_and_preserves_changed_target() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let source = source("review-identity");
                let state = Arc::new(Mutex::new(RemoteState {
                    files: HashMap::from([("/a".into(), b"reviewed old".to_vec())]),
                    ..Default::default()
                }));
                let mut manager =
                    SftpTransferManager::from_remote_endpoint(ScriptedRemote(state.clone()));
                manager
                    .enqueue_batch(vec![SftpTransferRequest::new(
                        SftpPath::local(&source),
                        SftpPath::remote("/a"),
                    )
                    .unwrap()])
                    .unwrap();
                let collision = loop {
                    if let SftpTransferEvent::Collision(value) = next_event(&mut manager).await {
                        break value;
                    }
                };
                state
                    .lock()
                    .unwrap()
                    .files
                    .insert("/a".into(), b"unrelated replacement".to_vec());
                resolve(&manager, &collision, SftpCollisionDecision::Replace);
                loop {
                    if matches!(
                        next_event(&mut manager).await,
                        SftpTransferEvent::BatchFinished { .. }
                    ) {
                        break;
                    }
                }
                assert_eq!(
                    state.lock().unwrap().events,
                    ["review", "upload", "release"]
                );
                assert_eq!(state.lock().unwrap().files["/a"], b"unrelated replacement");
                manager.shutdown().await.unwrap();
            });
    }

    #[test]
    fn rooted_queue_lost_finish_is_uncertain_and_is_never_replayed() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let source = source("uncertain-finish");
                let state = Arc::new(Mutex::new(RemoteState {
                    uncertain: true,
                    ..Default::default()
                }));
                let mut manager =
                    SftpTransferManager::from_remote_endpoint(ScriptedRemote(state.clone()));
                manager
                    .enqueue_batch(vec![SftpTransferRequest::new(
                        SftpPath::local(&source),
                        SftpPath::remote("/a"),
                    )
                    .unwrap()])
                    .unwrap();
                let mut uncertain = 0;
                loop {
                    match next_event(&mut manager).await {
                        SftpTransferEvent::ItemPublicationUncertain { .. } => uncertain += 1,
                        SftpTransferEvent::ItemFailed { .. }
                        | SftpTransferEvent::ItemCompleted { .. } => {
                            panic!("uncertain finish must retain its own outcome")
                        }
                        SftpTransferEvent::BatchFinished { .. } => break,
                        _ => {}
                    }
                }
                assert_eq!(uncertain, 1);
                assert_eq!(state.lock().unwrap().events, ["upload"]);
                assert_eq!(state.lock().unwrap().files["/a"], b"new contents");
                manager.shutdown().await.unwrap();
            });
    }

    #[derive(Default)]
    struct LocalState {
        files: HashMap<PathBuf, Vec<u8>>,
        review: Option<(PathBuf, Vec<u8>)>,
        created: usize,
        cleaned: usize,
        published: usize,
        late_collision: bool,
        stall_write: bool,
    }
    struct RetainedDestinations(Arc<Mutex<LocalState>>);
    struct NoUploadFallback;
    impl LocalUploadSources for NoUploadFallback {
        fn metadata(&self, _: &Path) -> Result<SftpPathMetadata, SftpSessionError> {
            panic!("download metadata must use retained destination capability")
        }
        fn open(&self, _: &Path) -> Result<OpenedLocalUpload, SftpSessionError> {
            panic!("download must never open an upload source")
        }
    }
    impl LocalDownloadDestinations for RetainedDestinations {
        fn validate_source(&self, source: &SftpPathMetadata) -> Result<(), SftpSessionError> {
            if source.file_type != SftpEntryType::File
                || source.size.is_none_or(|n| n > 16 * 1024 * 1024)
            {
                return Err(endpoint_error(
                    "download",
                    &source.path.display(),
                    "regular file cap",
                ));
            }
            Ok(())
        }
        fn transfer_timeout(&self) -> std::time::Duration {
            if self.0.lock().unwrap().stall_write {
                std::time::Duration::from_millis(10)
            } else {
                std::time::Duration::from_secs(1)
            }
        }
        fn metadata(&self, path: &Path) -> Result<Option<SftpPathMetadata>, SftpSessionError> {
            let state = self.0.lock().unwrap();
            let metadata = if path == Path::new("/retained") {
                Some((SftpEntryType::Directory, 0))
            } else {
                state
                    .files
                    .get(path)
                    .map(|bytes| (SftpEntryType::File, bytes.len() as u64))
            };
            Ok(metadata.map(|(kind, size)| SftpPathMetadata {
                path: SftpPath::local(path),
                file_type: kind,
                size: Some(size),
                modified_at: None,
                permissions: Some(0o600),
            }))
        }
        fn review_replacement(
            &mut self,
            path: &Path,
        ) -> Result<(LocalReplacementReview, SftpPathMetadata), SftpSessionError> {
            let metadata = self.metadata(path)?.unwrap();
            let mut state = self.0.lock().unwrap();
            assert!(state.review.is_none());
            state.review = Some((path.to_owned(), state.files[path].clone()));
            Ok((LocalReplacementReview(9), metadata))
        }
        fn release_replacement(
            &mut self,
            review: LocalReplacementReview,
        ) -> Result<(), SftpSessionError> {
            assert_eq!(review, LocalReplacementReview(9));
            assert!(self.0.lock().unwrap().review.take().is_some());
            Ok(())
        }
        fn create(
            &mut self,
            path: &Path,
            expected: Option<LocalReplacementReview>,
        ) -> Result<Box<dyn LocalDownloadFile>, SftpSessionError> {
            assert!(path.starts_with("/retained"));
            self.0.lock().unwrap().created += 1;
            Ok(Box::new(RetainedFile {
                state: self.0.clone(),
                target: path.to_owned(),
                expected,
                bytes: Vec::new(),
                flushed: false,
            }))
        }
    }
    struct RetainedFile {
        state: Arc<Mutex<LocalState>>,
        target: PathBuf,
        expected: Option<LocalReplacementReview>,
        bytes: Vec<u8>,
        flushed: bool,
    }
    impl AsyncWrite for RetainedFile {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            if self.state.lock().unwrap().stall_write {
                return std::task::Poll::Pending;
            }
            self.bytes.extend_from_slice(bytes);
            self.flushed = false;
            std::task::Poll::Ready(Ok(bytes.len()))
        }
        fn poll_flush(
            mut self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            self.flushed = true;
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }
    impl LocalDownloadFile for RetainedFile {
        fn publish(&mut self) -> Result<(), SftpSessionError> {
            assert!(
                self.flushed,
                "queue must flush its retained writer before publication"
            );
            let mut state = self.state.lock().unwrap();
            if std::mem::take(&mut state.late_collision) {
                state
                    .files
                    .insert(self.target.clone(), b"late original".to_vec());
            }
            if let Some(review) = self.expected {
                assert_eq!(review, LocalReplacementReview(9));
                let (path, original) = state.review.as_ref().unwrap();
                assert_eq!(path, &self.target);
                if state.files.get(path) != Some(original) {
                    return Err(endpoint_error(
                        "publish",
                        &self.target.display().to_string(),
                        "reviewed target changed",
                    ));
                }
            } else if state.files.contains_key(&self.target) {
                return Err(SftpSessionError::DestinationExists {
                    path: self.target.display().to_string(),
                });
            }
            state.files.insert(self.target.clone(), self.bytes.clone());
            state.published += 1;
            Ok(())
        }
        fn cleanup(&mut self) -> Result<(), SftpSessionError> {
            self.state.lock().unwrap().cleaned += 1;
            Ok(())
        }
    }
    fn retained_manager(local: Arc<Mutex<LocalState>>) -> SftpTransferManager {
        let remote = ScriptedRemote(Arc::new(Mutex::new(RemoteState {
            files: HashMap::from([("/file".into(), b"incoming bytes".to_vec())]),
            ..Default::default()
        })));
        SftpTransferManager::from_remote_endpoint_with_local_files(
            remote,
            Arc::new(NoUploadFallback),
            RetainedDestinations(local),
        )
    }
    #[test]
    fn retained_local_queue_skip_keep_both_replace_flush_and_no_path_fallback() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                for decision in [
                    SftpCollisionDecision::Skip,
                    SftpCollisionDecision::KeepBoth,
                    SftpCollisionDecision::Replace,
                ] {
                    let local = Arc::new(Mutex::new(LocalState {
                        files: HashMap::from([(
                            PathBuf::from("/retained/file"),
                            b"original".to_vec(),
                        )]),
                        ..Default::default()
                    }));
                    let mut manager = retained_manager(local.clone());
                    manager
                        .enqueue_batch(vec![SftpTransferRequest::new(
                            SftpPath::remote("/file"),
                            SftpPath::local("/retained"),
                        )
                        .unwrap()])
                        .unwrap();
                    loop {
                        match next_event(&mut manager).await {
                            SftpTransferEvent::Collision(collision) => {
                                assert_eq!(
                                    collision.destination.path,
                                    SftpPath::local("/retained/file")
                                );
                                assert_eq!(collision.destination.size, Some(8));
                                resolve(&manager, &collision, decision);
                            }
                            SftpTransferEvent::ItemFailed { reason, .. }
                            | SftpTransferEvent::ItemPublicationUncertain { reason, .. } => {
                                panic!("{reason}")
                            }
                            SftpTransferEvent::BatchFinished { .. } => break,
                            _ => {}
                        }
                    }
                    manager.shutdown().await.unwrap();
                    let state = local.lock().unwrap();
                    assert!(state.review.is_none());
                    if decision == SftpCollisionDecision::Skip {
                        assert_eq!(state.created, 0);
                        assert_eq!(state.published, 0);
                    } else {
                        assert_eq!(state.created, 1);
                        assert_eq!(state.cleaned, 1);
                        assert_eq!(state.published, 1);
                    }
                    assert_eq!(
                        state.files[Path::new("/retained/file")],
                        if decision == SftpCollisionDecision::Replace {
                            b"incoming bytes".to_vec()
                        } else {
                            b"original".to_vec()
                        }
                    );
                    if decision == SftpCollisionDecision::KeepBoth {
                        assert_eq!(state.files.len(), 2);
                        assert!(state
                            .files
                            .iter()
                            .any(|(path, bytes)| path != Path::new("/retained/file")
                                && bytes.as_slice() == b"incoming bytes"));
                    }
                }
            });
    }
    #[test]
    fn retained_local_queue_changed_review_refuses_without_unlinking_target() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let local = Arc::new(Mutex::new(LocalState {
                    files: HashMap::from([(PathBuf::from("/retained/file"), b"original".to_vec())]),
                    ..Default::default()
                }));
                let mut manager = retained_manager(local.clone());
                manager
                    .enqueue_batch(vec![SftpTransferRequest::new(
                        SftpPath::remote("/file"),
                        SftpPath::local("/retained"),
                    )
                    .unwrap()])
                    .unwrap();
                let mut failed = false;
                loop {
                    match next_event(&mut manager).await {
                        SftpTransferEvent::Collision(collision) => {
                            local
                                .lock()
                                .unwrap()
                                .files
                                .insert(PathBuf::from("/retained/file"), b"replacement".to_vec());
                            resolve(&manager, &collision, SftpCollisionDecision::Replace);
                        }
                        SftpTransferEvent::ItemFailed { .. } => failed = true,
                        SftpTransferEvent::BatchFinished { .. } => break,
                        _ => {}
                    }
                }
                manager.shutdown().await.unwrap();
                let state = local.lock().unwrap();
                assert!(failed);
                assert_eq!(state.published, 0);
                assert_eq!(state.cleaned, 1);
                assert_eq!(state.files[Path::new("/retained/file")], b"replacement");
            });
    }
    #[test]
    fn retained_local_queue_late_collision_cleans_stage_before_fresh_review() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                for decision in [
                    SftpCollisionDecision::Skip,
                    SftpCollisionDecision::KeepBoth,
                    SftpCollisionDecision::Replace,
                ] {
                    let local = Arc::new(Mutex::new(LocalState {
                        late_collision: true,
                        ..Default::default()
                    }));
                    let mut manager = retained_manager(local.clone());
                    manager
                        .enqueue_batch(vec![SftpTransferRequest::new(
                            SftpPath::remote("/file"),
                            SftpPath::local("/retained"),
                        )
                        .unwrap()])
                        .unwrap();
                    let mut reviewed = false;
                    let mut completed = false;
                    let size = b"incoming bytes".len() as u64;
                    loop {
                        match next_event(&mut manager).await {
                            SftpTransferEvent::Collision(collision) => {
                                assert_eq!(local.lock().unwrap().cleaned, 1);
                                reviewed = true;
                                resolve(&manager, &collision, decision);
                            }
                            SftpTransferEvent::ItemProgress {
                                bytes_transferred,
                                total_bytes,
                                ..
                            } => {
                                assert_eq!(total_bytes, Some(size));
                                assert!(
                                    bytes_transferred <= size,
                                    "retry progress counted an unpublished attempt twice"
                                );
                            }
                            SftpTransferEvent::ItemCompleted {
                                bytes_transferred,
                                total_bytes,
                                ..
                            } => {
                                assert_eq!(bytes_transferred, size);
                                assert_eq!(total_bytes, Some(size));
                                completed = true;
                            }
                            SftpTransferEvent::ItemFailed { reason, .. }
                            | SftpTransferEvent::ItemPublicationUncertain { reason, .. } => {
                                panic!("{reason}")
                            }
                            SftpTransferEvent::BatchFinished { .. } => break,
                            _ => {}
                        }
                    }
                    manager.shutdown().await.unwrap();
                    let state = local.lock().unwrap();
                    assert!(reviewed);
                    assert!(state.review.is_none());
                    assert_eq!(completed, decision != SftpCollisionDecision::Skip);
                    let attempts = if decision == SftpCollisionDecision::Skip {
                        1
                    } else {
                        2
                    };
                    assert_eq!(state.created, attempts);
                    assert_eq!(state.cleaned, attempts);
                    assert_eq!(
                        state.published,
                        usize::from(decision != SftpCollisionDecision::Skip)
                    );
                    assert_eq!(
                        state.files[Path::new("/retained/file")],
                        if decision == SftpCollisionDecision::Replace {
                            b"incoming bytes".as_slice()
                        } else {
                            b"late original".as_slice()
                        }
                    );
                    assert_eq!(
                        state.files.len(),
                        if decision == SftpCollisionDecision::KeepBoth {
                            2
                        } else {
                            1
                        }
                    );
                    if decision == SftpCollisionDecision::KeepBoth {
                        assert!(state
                            .files
                            .iter()
                            .any(|(path, bytes)| path != Path::new("/retained/file")
                                && bytes.as_slice() == b"incoming bytes"));
                    }
                }
            });
    }
    #[test]
    fn retained_local_source_policy_refuses_directory_before_planner_enumeration() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let local = Arc::new(Mutex::new(LocalState::default()));
                let mut manager = retained_manager(local.clone());
                manager
                    .enqueue_batch(vec![SftpTransferRequest::new(
                        SftpPath::remote("/"),
                        SftpPath::local("/retained"),
                    )
                    .unwrap()])
                    .unwrap();
                let mut failed = false;
                loop {
                    match next_event(&mut manager).await {
                        SftpTransferEvent::ItemFailed { .. } => failed = true,
                        SftpTransferEvent::BatchFinished { .. } => break,
                        _ => {}
                    }
                }
                manager.shutdown().await.unwrap();
                assert!(failed);
                assert_eq!(local.lock().unwrap().created, 0);
            });
    }

    #[test]
    fn retained_local_queue_timeout_keeps_stage_owned_until_cleanup() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let local = Arc::new(Mutex::new(LocalState {
                    stall_write: true,
                    ..Default::default()
                }));
                let mut manager = retained_manager(local.clone());
                manager
                    .enqueue_batch(vec![SftpTransferRequest::new(
                        SftpPath::remote("/file"),
                        SftpPath::local("/retained"),
                    )
                    .unwrap()])
                    .unwrap();
                let mut failed = false;
                loop {
                    match next_event(&mut manager).await {
                        SftpTransferEvent::ItemFailed { reason, .. } => {
                            assert!(reason.contains("timed out"));
                            failed = true;
                        }
                        SftpTransferEvent::BatchFinished { .. } => break,
                        _ => {}
                    }
                }
                manager.shutdown().await.unwrap();
                let state = local.lock().unwrap();
                assert!(failed);
                assert_eq!(state.created, 1);
                assert_eq!(state.cleaned, 1);
                assert_eq!(state.published, 0);
                assert!(state.files.is_empty());
            });
    }
}
