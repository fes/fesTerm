//! Retained Documents destination for the shared transfer queue. No path-open fallback.
use super::*;
use festerm_ssh::{
    LocalDownloadDestinations, LocalDownloadFile, LocalReplacementReview, SftpPathMetadata,
    SftpSessionError,
};
#[cfg(unix)]
use festerm_ssh::{SftpEntryType, SftpPath};
use std::path::{Path, PathBuf};

pub(super) fn directory(root: &local_root::Root, relative: &str) -> Result<PathBuf, String> {
    #[cfg(unix)]
    {
        root.check().map_err(|_| "App Documents changed")?;
        let relative = path::normalize_local(relative).map_err(|_| "Invalid local folder")?;
        Ok(root.root().join(relative))
    }
    #[cfg(not(unix))]
    {
        let _ = (root, relative);
        Err("Downloads are unavailable".into())
    }
}

fn refused() -> SftpSessionError {
    SftpSessionError::LocalOperationFailed {
        operation: "write admitted download destination",
        path: "Documents".into(),
        reason: "The destination or its authority changed. Review it again.".into(),
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::{fs::File, os::unix::fs::MetadataExt};
    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Stamp {
        device: u64,
        inode: u64,
        size: u64,
        modified: (i64, i64),
        changed: (i64, i64),
    }
    fn stamp(file: &File) -> Result<Stamp, SftpSessionError> {
        let m = file.metadata().map_err(|_| refused())?;
        if !m.is_file()
            || m.nlink() != 1
            || m.mode() & 0o022 != 0
            || m.uid() != festerm_unix_security::current_user_id().map_err(|_| refused())?
        {
            return Err(refused());
        }
        Ok(Stamp {
            device: m.dev(),
            inode: m.ino(),
            size: m.len(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
        })
    }
    struct Review {
        path: PathBuf,
        file: File,
        stamp: Stamp,
        security: festerm_unix_security::SecurityMetadata,
    }
    pub(crate) struct Destinations {
        root: Arc<local_root::Root>,
        current: Arc<dyn Fn() -> Result<(), SftpError> + Send + Sync>,
        cancel: CancellationToken,
        next: u64,
        review: Option<(LocalReplacementReview, Arc<Review>)>,
    }
    impl Destinations {
        pub(crate) fn new(
            root: Arc<local_root::Root>,
            current: Arc<dyn Fn() -> Result<(), SftpError> + Send + Sync>,
            cancel: CancellationToken,
        ) -> Self {
            Self {
                root,
                current,
                cancel,
                next: 0,
                review: None,
            }
        }
        fn check(&self) -> Result<(), SftpSessionError> {
            if self.cancel.is_cancelled() {
                return Err(refused());
            }
            (self.current)().map_err(|_| refused())?;
            self.root.check().map_err(|_| refused())
        }
        fn relative<'a>(&self, path: &'a Path) -> Result<&'a str, SftpSessionError> {
            let relative = path
                .strip_prefix(self.root.root())
                .map_err(|_| refused())?
                .to_str()
                .ok_or_else(refused)?;
            path::local_components(relative).map_err(|_| refused())?;
            Ok(relative)
        }
        fn parent(&self, path: &Path) -> Result<(File, PathBuf), SftpSessionError> {
            self.check()?;
            let relative = self.relative(path)?;
            let mut components = path::local_components(relative).map_err(|_| refused())?;
            let name = components.pop().ok_or_else(refused)?;
            let mut parent = self.root.directory().map_err(|_| refused())?;
            for name in components {
                parent =
                    festerm_unix_security::open_directory_read_nofollow(&parent, Path::new(name))
                        .map_err(|_| refused())?;
                festerm_unix_security::trusted_private_storage_ancestor(&parent)
                    .map_err(|_| refused())?;
            }
            Ok((parent, PathBuf::from(name)))
        }
        fn metadata_for(
            &self,
            path: &Path,
            file: &File,
            file_type: SftpEntryType,
        ) -> Result<SftpPathMetadata, SftpSessionError> {
            let m = file.metadata().map_err(|_| refused())?;
            Ok(SftpPathMetadata {
                path: SftpPath::local(path),
                file_type,
                size: if m.is_file() { Some(m.len()) } else { None },
                modified_at: m.modified().ok(),
                permissions: None,
            })
        }
    }
    impl LocalDownloadDestinations for Destinations {
        fn validate_source(&self, metadata: &SftpPathMetadata) -> Result<(), SftpSessionError> {
            self.check()?;
            if metadata.file_type != SftpEntryType::File
                || metadata.size.is_none_or(|size| size > MAX_DOWNLOAD)
            {
                return Err(refused());
            }
            Ok(())
        }
        fn transfer_timeout(&self) -> Duration {
            DEADLINE
        }
        fn metadata(&self, path: &Path) -> Result<Option<SftpPathMetadata>, SftpSessionError> {
            self.check()?;
            if path == self.root.root() {
                return self
                    .metadata_for(
                        path,
                        &self.root.directory().map_err(|_| refused())?,
                        SftpEntryType::Directory,
                    )
                    .map(Some);
            }
            let (parent, name) = self.parent(path)?;
            match festerm_unix_security::open_file_nofollow(&parent, &name) {
                Ok(file) => self
                    .metadata_for(path, &file, SftpEntryType::File)
                    .map(Some),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(_) => {
                    let dir = festerm_unix_security::open_directory_read_nofollow(&parent, &name)
                        .map_err(|_| refused())?;
                    self.metadata_for(path, &dir, SftpEntryType::Directory)
                        .map(Some)
                }
            }
        }
        fn review_replacement(
            &mut self,
            path: &Path,
        ) -> Result<(LocalReplacementReview, SftpPathMetadata), SftpSessionError> {
            if self.review.is_some() {
                return Err(refused());
            }
            let (parent, name) = self.parent(path)?;
            let file =
                festerm_unix_security::open_file_nofollow(&parent, &name).map_err(|_| refused())?;
            let review = Review {
                path: path.to_owned(),
                stamp: stamp(&file)?,
                security: festerm_unix_security::security_metadata(&file).map_err(|_| refused())?,
                file,
            };
            let metadata = self.metadata_for(path, &review.file, SftpEntryType::File)?;
            self.next = self.next.checked_add(1).ok_or_else(refused)?;
            let token = LocalReplacementReview(self.next);
            self.review = Some((token, Arc::new(review)));
            Ok((token, metadata))
        }
        fn release_replacement(
            &mut self,
            token: LocalReplacementReview,
        ) -> Result<(), SftpSessionError> {
            if self.review.as_ref().is_none_or(|(id, _)| *id != token) {
                return Err(refused());
            }
            self.review = None;
            Ok(())
        }
        fn create(
            &mut self,
            destination: &Path,
            expected: Option<LocalReplacementReview>,
        ) -> Result<Box<dyn LocalDownloadFile>, SftpSessionError> {
            self.check()?;
            let relative = self.relative(destination)?;
            let parent = Path::new(relative)
                .parent()
                .and_then(Path::to_str)
                .ok_or_else(refused)?;
            let name = destination
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(refused)?
                .to_owned();
            let review = if let Some(expected) = expected {
                let (id, review) = self.review.as_ref().ok_or_else(refused)?;
                if *id != expected || review.path != destination {
                    return Err(refused());
                }
                Some(review.clone())
            } else {
                None
            };
            let stage = local_download::Stage::create(&self.root, parent).map_err(|_| refused())?;
            let file = stage.writer().map_err(|_| refused())?;
            Ok(Box::new(Output {
                stage,
                file,
                name,
                destination: destination.to_owned(),
                root: self.root.clone(),
                current: self.current.clone(),
                cancel: self.cancel.clone(),
                review,
                written: 0,
            }))
        }
    }
    struct Output {
        stage: local_download::Stage,
        file: tokio::fs::File,
        name: String,
        destination: PathBuf,
        root: Arc<local_root::Root>,
        current: Arc<dyn Fn() -> Result<(), SftpError> + Send + Sync>,
        cancel: CancellationToken,
        review: Option<Arc<Review>>,
        written: u64,
    }
    impl Output {
        fn check(&self) -> Result<(), SftpSessionError> {
            if self.cancel.is_cancelled() {
                return Err(refused());
            }
            (self.current)().map_err(|_| refused())?;
            self.root.check().map_err(|_| refused())
        }
        fn check_review(&self) -> Result<(), SftpSessionError> {
            self.check()?;
            if let Some(review) = &self.review {
                if stamp(&review.file)? != review.stamp
                    || !festerm_unix_security::security_metadata_matches(
                        &review.file,
                        &review.security,
                    )
                    .map_err(|_| refused())?
                {
                    return Err(refused());
                }
            }
            Ok(())
        }
    }
    impl AsyncWrite for Output {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            if self.check().is_err() {
                return Poll::Ready(Err(std::io::ErrorKind::PermissionDenied.into()));
            }
            if bytes.len() as u64 > MAX_DOWNLOAD.saturating_sub(self.written) {
                return Poll::Ready(Err(std::io::ErrorKind::FileTooLarge.into()));
            }
            let n = std::task::ready!(Pin::new(&mut self.file).poll_write(cx, bytes))?;
            self.written += n as u64;
            Poll::Ready(Ok(n))
        }
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.file).poll_flush(cx)
        }
        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.file).poll_shutdown(cx)
        }
    }
    impl LocalDownloadFile for Output {
        fn publish(&mut self) -> Result<(), SftpSessionError> {
            self.check_review()?;
            let current = self.current.clone();
            let cancel = self.cancel.clone();
            let review = self.review.clone();
            self.stage
                .publish_reviewed(&self.name, self.review.as_ref().map(|r| &r.file), || {
                    if cancel.is_cancelled() {
                        return Err(SftpError::Cancelled);
                    }
                    current()?;
                    if let Some(review) = &review {
                        if stamp(&review.file).map_err(|_| denied())? != review.stamp
                            || !festerm_unix_security::security_metadata_matches(
                                &review.file,
                                &review.security,
                            )
                            .map_err(|_| denied())?
                        {
                            return Err(denied());
                        }
                    }
                    Ok(())
                })
                .map_err(|e| {
                    if matches!(e, SftpError::AlreadyExists(_)) {
                        SftpSessionError::DestinationExists {
                            path: self.destination.display().to_string(),
                        }
                    } else {
                        refused()
                    }
                })
        }
        fn cleanup(&mut self) -> Result<(), SftpSessionError> {
            self.stage.cleanup().map_err(|_| refused())
        }
    }
}
#[cfg(unix)]
pub(super) use platform::Destinations;

#[cfg(not(unix))]
pub(super) struct Destinations;
#[cfg(not(unix))]
impl Destinations {
    pub(super) fn new(
        _: Arc<local_root::Root>,
        _: Arc<dyn Fn() -> Result<(), SftpError> + Send + Sync>,
        _: CancellationToken,
    ) -> Self {
        Self
    }
}
#[cfg(not(unix))]
impl LocalDownloadDestinations for Destinations {
    fn validate_source(&self, _: &SftpPathMetadata) -> Result<(), SftpSessionError> {
        Err(refused())
    }
    fn transfer_timeout(&self) -> Duration {
        DEADLINE
    }
    fn metadata(&self, _: &Path) -> Result<Option<SftpPathMetadata>, SftpSessionError> {
        Err(refused())
    }
    fn review_replacement(
        &mut self,
        _: &Path,
    ) -> Result<(LocalReplacementReview, SftpPathMetadata), SftpSessionError> {
        Err(refused())
    }
    fn release_replacement(&mut self, _: LocalReplacementReview) -> Result<(), SftpSessionError> {
        Err(refused())
    }
    fn create(
        &mut self,
        _: &Path,
        _: Option<LocalReplacementReview>,
    ) -> Result<Box<dyn LocalDownloadFile>, SftpSessionError> {
        Err(refused())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::AsyncWriteExt;

    #[test]
    fn host_download_destination_caps_sources_and_actual_bytes_and_retires_authority() {
        let base = tempfile::tempdir().unwrap();
        let root = local_root::Root::open(base.path()).unwrap();
        let current = Arc::new(AtomicBool::new(true));
        let checked = current.clone();
        let mut destinations = Destinations::new(
            root.clone(),
            Arc::new(move || {
                if checked.load(Ordering::SeqCst) {
                    Ok(())
                } else {
                    Err(SftpError::Cancelled)
                }
            }),
            CancellationToken::new(),
        );
        let mut metadata = SftpPathMetadata {
            path: SftpPath::remote("/source"),
            file_type: SftpEntryType::File,
            size: Some(MAX_DOWNLOAD),
            modified_at: None,
            permissions: None,
        };
        assert!(destinations.validate_source(&metadata).is_ok());
        metadata.size = Some(MAX_DOWNLOAD + 1);
        assert!(destinations.validate_source(&metadata).is_err());
        metadata.size = None;
        assert!(destinations.validate_source(&metadata).is_err());
        metadata.size = Some(1);
        metadata.file_type = SftpEntryType::Directory;
        assert!(destinations.validate_source(&metadata).is_err());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut output = destinations
                .create(&root.root().join("large"), None)
                .unwrap();
            assert!(output
                .write_all(&vec![0; MAX_DOWNLOAD as usize + 1])
                .await
                .is_err());
            output.cleanup().unwrap();
            let mut output = destinations
                .create(&root.root().join("retired"), None)
                .unwrap();
            output.write_all(b"public").await.unwrap();
            output.flush().await.unwrap();
            current.store(false, Ordering::SeqCst);
            assert!(output.write_all(b"unadmitted").await.is_err());
            assert!(output.publish().is_err());
            output.cleanup().unwrap();
        });
        assert_eq!(std::fs::read_dir(base.path()).unwrap().count(), 0);
    }

    #[test]
    fn host_download_review_refuses_in_place_edit_and_alias_without_losing_originals() {
        use std::os::unix::fs::symlink;
        let base = tempfile::tempdir().unwrap();
        let root = local_root::Root::open(base.path()).unwrap();
        let mut destinations =
            Destinations::new(root.clone(), Arc::new(|| Ok(())), CancellationToken::new());
        let target = root.root().join("target");
        std::fs::write(&target, b"old").unwrap();
        let (review, _) = destinations.review_replacement(&target).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut output = destinations.create(&target, Some(review)).unwrap();
            output.write_all(b"incoming").await.unwrap();
            output.flush().await.unwrap();
            std::fs::write(&target, b"modified after review").unwrap();
            assert!(output.publish().is_err());
            output.cleanup().unwrap();
        });
        destinations.release_replacement(review).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"modified after review");
        symlink(&target, root.root().join("alias")).unwrap();
        assert!(destinations.metadata(&root.root().join("alias")).is_err());
        assert!(destinations
            .review_replacement(&root.root().join("alias"))
            .is_err());
        assert_eq!(std::fs::read_dir(base.path()).unwrap().count(), 2);
    }
}
