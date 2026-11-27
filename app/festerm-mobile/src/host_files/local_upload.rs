//! Caller-admitted Documents files for the existing neutral transfer queue.
//! Retained objects and per-read negative fences; no same-user race immunity.
use super::*;
use festerm_ssh::{
    LocalUploadSources, OpenedLocalUpload, SftpPath, SftpPathMetadata, SftpSessionError,
};
use std::path::{Path, PathBuf};

fn refused() -> SftpSessionError {
    SftpSessionError::LocalUploadSourceUnavailable {
        operation: "read admitted upload source",
        path: "Documents".into(),
        reason: "The source or its authority changed. Select the file again.".into(),
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::{
        fs::File,
        io::{Seek, SeekFrom},
        os::unix::fs::MetadataExt,
    };
    #[derive(Clone, Copy, Eq, PartialEq)]
    struct SourceStamp {
        device: u64,
        inode: u64,
        size: u64,
        modified: (i64, i64),
        changed: (i64, i64),
    }
    struct Held {
        root: Arc<local_root::Root>,
        parent: File,
        components: Vec<String>,
        name: PathBuf,
        file: File,
        stamp: SourceStamp,
        security: festerm_unix_security::SecurityMetadata,
        current: Arc<dyn Fn() -> Result<(), SftpError> + Send + Sync>,
    }
    fn stamp(file: &File) -> Result<SourceStamp, SftpSessionError> {
        let m = file.metadata().map_err(|_| refused())?;
        if !m.is_file()
            || m.nlink() != 1
            || m.mode() & 0o022 != 0
            || m.uid() != festerm_unix_security::current_user_id().map_err(|_| refused())?
        {
            return Err(refused());
        }
        Ok(SourceStamp {
            device: m.dev(),
            inode: m.ino(),
            size: m.len(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
        })
    }
    fn same(a: &File, b: &File) -> Result<bool, SftpSessionError> {
        let a = a.metadata().map_err(|_| refused())?;
        let b = b.metadata().map_err(|_| refused())?;
        Ok(a.dev() == b.dev() && a.ino() == b.ino())
    }
    impl Held {
        fn check(&self) -> Result<(), SftpSessionError> {
            (self.current)().map_err(|_| refused())?;
            let mut parent = self.root.directory().map_err(|_| refused())?;
            festerm_unix_security::trusted_private_storage_ancestor(&parent)
                .map_err(|_| refused())?;
            for component in &self.components {
                parent = festerm_unix_security::open_directory_read_nofollow(
                    &parent,
                    Path::new(component),
                )
                .map_err(|_| refused())?;
                festerm_unix_security::trusted_private_storage_ancestor(&parent)
                    .map_err(|_| refused())?;
            }
            if !same(&parent, &self.parent)?
                || !festerm_unix_security::download_file_matches(&parent, &self.name, &self.file)
                    .map_err(|_| refused())?
                || stamp(&self.file)? != self.stamp
                || !festerm_unix_security::security_metadata_matches(&self.file, &self.security)
                    .map_err(|_| refused())?
            {
                return Err(refused());
            }
            Ok(())
        }
        fn metadata(&self, path: &Path) -> Result<SftpPathMetadata, SftpSessionError> {
            self.check()?;
            Ok(SftpPathMetadata {
                path: SftpPath::local(path),
                file_type: festerm_ssh::SftpEntryType::File,
                size: Some(self.stamp.size),
                modified_at: self.file.metadata().map_err(|_| refused())?.modified().ok(),
                permissions: None,
            })
        }
    }
    pub(crate) struct Sources {
        root: Arc<local_root::Root>,
        current: Arc<dyn Fn() -> Result<(), SftpError> + Send + Sync>,
        held: Mutex<HashMap<PathBuf, Arc<Held>>>,
    }
    impl Sources {
        pub(crate) fn new(
            root: Arc<local_root::Root>,
            current: Arc<dyn Fn() -> Result<(), SftpError> + Send + Sync>,
        ) -> Self {
            Self {
                root,
                current,
                held: Mutex::new(HashMap::new()),
            }
        }
        pub(crate) fn admit(&self, relative: &str) -> Result<PathBuf, SftpSessionError> {
            (self.current)().map_err(|_| refused())?;
            let mut components = path::local_components(relative)
                .map_err(|_| refused())?
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let name = PathBuf::from(components.pop().ok_or_else(refused)?);
            let path = self
                .root
                .root()
                .join(path::normalize_local(relative).map_err(|_| refused())?);
            let mut held = self.held.lock().map_err(|_| refused())?;
            if held.len() >= QUEUE || held.contains_key(&path) {
                return Err(refused());
            }
            let mut parent = self.root.directory().map_err(|_| refused())?;
            festerm_unix_security::trusted_private_storage_ancestor(&parent)
                .map_err(|_| refused())?;
            for component in &components {
                parent = festerm_unix_security::open_directory_read_nofollow(
                    &parent,
                    Path::new(component),
                )
                .map_err(|_| refused())?;
                festerm_unix_security::trusted_private_storage_ancestor(&parent)
                    .map_err(|_| refused())?;
            }
            let file =
                festerm_unix_security::open_file_nofollow(&parent, &name).map_err(|_| refused())?;
            let source = Held {
                root: self.root.clone(),
                parent,
                components,
                name,
                stamp: stamp(&file)?,
                security: festerm_unix_security::security_metadata(&file).map_err(|_| refused())?,
                file,
                current: self.current.clone(),
            };
            source.check()?;
            held.insert(path.clone(), Arc::new(source));
            Ok(path)
        }
        pub(crate) fn mkdir(&self, relative: &str) -> Result<(), SftpError> {
            (self.current)()?;
            let mut components = path::local_components(relative)?
                .into_iter()
                .collect::<Vec<_>>();
            let name = components.pop().ok_or_else(denied)?;
            let mut parent = self.root.directory()?;
            festerm_unix_security::trusted_private_storage_ancestor(&parent)
                .map_err(|_| denied())?;
            for component in components {
                parent = festerm_unix_security::open_directory_read_nofollow(
                    &parent,
                    Path::new(component),
                )
                .map_err(|_| denied())?;
                festerm_unix_security::trusted_private_storage_ancestor(&parent)
                    .map_err(|_| denied())?;
            }
            self.root.check()?;
            (self.current)()?;
            festerm_unix_security::create_private_directory(&parent, Path::new(name)).map_err(
                |e| {
                    if e.kind() == std::io::ErrorKind::AlreadyExists {
                        SftpError::AlreadyExists("A file or folder already uses that name".into())
                    } else {
                        denied()
                    }
                },
            )?;
            self.root.check()?;
            (self.current)()
        }
        pub(crate) fn check_root(&self) -> Result<(), SftpError> {
            self.root.check()
        }
        pub(crate) fn release(&self, path: &Path) {
            self.held.lock().unwrap().remove(path);
        }
    }
    struct Reader {
        held: Arc<Held>,
        file: tokio::fs::File,
    }
    impl AsyncRead for Reader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if self.held.check().is_err() {
                return Poll::Ready(Err(std::io::ErrorKind::Interrupted.into()));
            }
            let result = std::task::ready!(Pin::new(&mut self.file).poll_read(cx, buf));
            if self.held.check().is_err() {
                return Poll::Ready(Err(std::io::ErrorKind::Interrupted.into()));
            }
            Poll::Ready(result)
        }
    }
    impl LocalUploadSources for Sources {
        fn metadata(&self, path: &Path) -> Result<SftpPathMetadata, SftpSessionError> {
            self.held
                .lock()
                .map_err(|_| refused())?
                .get(path)
                .ok_or_else(refused)?
                .metadata(path)
        }
        fn open(&self, path: &Path) -> Result<OpenedLocalUpload, SftpSessionError> {
            let held = self
                .held
                .lock()
                .map_err(|_| refused())?
                .get(path)
                .ok_or_else(refused)?
                .clone();
            held.check()?;
            let mut file = held.file.try_clone().map_err(|_| refused())?;
            file.seek(SeekFrom::Start(0)).map_err(|_| refused())?;
            let size = held.stamp.size;
            Ok(OpenedLocalUpload {
                reader: Box::pin(Reader {
                    held,
                    file: tokio::fs::File::from_std(file),
                }),
                size,
            })
        }
    }
}
#[cfg(unix)]
pub(super) use platform::Sources;

#[cfg(not(unix))]
pub(super) struct Sources;
#[cfg(not(unix))]
impl Sources {
    pub(super) fn new(
        _: &LocalFs,
        _: Arc<dyn Fn() -> Result<(), SftpError> + Send + Sync>,
    ) -> Self {
        Self
    }
    pub(super) fn admit(&self, _: &str) -> Result<PathBuf, SftpSessionError> {
        Err(refused())
    }
    pub(super) fn check_root(&self) -> Result<(), SftpError> {
        Err(denied())
    }
    pub(super) fn mkdir(&self, _: &str) -> Result<(), SftpError> {
        Err(denied())
    }
    pub(super) fn release(&self, _: &Path) {}
}
#[cfg(not(unix))]
impl LocalUploadSources for Sources {
    fn metadata(&self, _: &Path) -> Result<SftpPathMetadata, SftpSessionError> {
        Err(refused())
    }
    fn open(&self, _: &Path) -> Result<OpenedLocalUpload, SftpSessionError> {
        Err(refused())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::symlink,
        sync::atomic::{AtomicBool, Ordering},
    };
    use tokio::io::AsyncReadExt;
    fn fixture() -> (tempfile::TempDir, LocalFs, Arc<AtomicBool>, Sources) {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("public.txt"), b"public source").unwrap();
        let local = LocalFs::new(root.path()).unwrap();
        let current = Arc::new(AtomicBool::new(true));
        let check = current.clone();
        let sources = Sources::new(
            local_root::Root::open(local.root()).unwrap(),
            Arc::new(move || {
                if check.load(Ordering::SeqCst) {
                    Ok(())
                } else {
                    Err(SftpError::Cancelled)
                }
            }),
        );
        (root, local, current, sources)
    }
    #[test]
    fn host_files_upload_source_replacement_and_size_change_refuse_held_authority() {
        let (_root, local, _, sources) = fixture();
        let path = sources.admit("public.txt").unwrap();
        assert_eq!(sources.metadata(&path).unwrap().size, Some(13));
        std::fs::rename(&path, local.root().join("retained.txt")).unwrap();
        std::fs::write(&path, b"wrong source!").unwrap();
        let error = sources.open(&path).err().unwrap();
        assert!(matches!(
            error,
            SftpSessionError::LocalUploadSourceUnavailable { .. }
        ));
        assert_eq!(
            error.to_string(),
            "local read admitted upload source failed for Documents: The source or its authority changed. Select the file again."
        );
        sources.release(&path);
        let path = sources.admit("public.txt").unwrap();
        std::fs::write(&path, b"changed").unwrap();
        assert!(sources.open(&path).is_err());
    }
    #[test]
    fn host_files_upload_source_refuses_alias_before_admission_and_parent_cutover() {
        let (_root, local, _, sources) = fixture();
        symlink(local.root().join("public.txt"), local.root().join("alias")).unwrap();
        assert!(sources.admit("alias").is_err());
        std::fs::create_dir(local.root().join("folder")).unwrap();
        std::fs::write(local.root().join("folder/a"), b"public").unwrap();
        let path = sources.admit("folder/a").unwrap();
        std::fs::rename(local.root().join("folder"), local.root().join("old")).unwrap();
        symlink(local.root().join("old"), local.root().join("folder")).unwrap();
        assert!(sources.open(&path).is_err());
    }
    #[test]
    fn host_files_upload_refuses_documents_root_alias_before_admission() {
        let root = tempfile::tempdir().unwrap();
        let selected = root.path().join("Documents");
        std::fs::create_dir(&selected).unwrap();
        std::fs::write(selected.join("public.txt"), b"public source").unwrap();
        let local = LocalFs::new(&selected).unwrap();
        let sources = Sources::new(
            local_root::Root::open(local.root()).unwrap(),
            Arc::new(|| Ok(())),
        );
        std::fs::rename(&selected, root.path().join("original")).unwrap();
        symlink(root.path().join("original"), &selected).unwrap();
        assert!(sources.admit("public.txt").is_err());
    }
    #[test]
    fn host_files_upload_refuses_hardlinks_and_writable_source_or_parent() {
        use std::os::unix::fs::PermissionsExt;
        let (_root, local, _, sources) = fixture();
        let source = local.root().join("public.txt");
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(sources.admit("public.txt").is_err());
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::hard_link(&source, local.root().join("another-name")).unwrap();
        assert!(sources.admit("public.txt").is_err());
        std::fs::remove_file(local.root().join("another-name")).unwrap();
        let admitted = sources.admit("public.txt").unwrap();
        std::fs::set_permissions(local.root(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let result = sources.open(&admitted);
        // Restore only this owned fixture for cleanup; production never repairs.
        std::fs::set_permissions(local.root(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
    }
    #[test]
    fn host_files_upload_held_reader_fences_account_and_content_each_chunk() {
        let (_root, _local, current, sources) = fixture();
        let path = sources.admit("public.txt").unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut source = sources.open(&path).unwrap();
            let mut bytes = [0; 2];
            source.reader.read_exact(&mut bytes).await.unwrap();
            assert_eq!(&bytes, b"pu");
            current.store(false, Ordering::SeqCst);
            assert!(source.reader.read_exact(&mut bytes).await.is_err());
        });
        current.store(true, Ordering::SeqCst);
        rt.block_on(async {
            let mut source = sources.open(&path).unwrap();
            let mut bytes = [0; 2];
            source.reader.read_exact(&mut bytes).await.unwrap();
            std::fs::write(&path, b"changed bytes").unwrap();
            assert!(source.reader.read_exact(&mut bytes).await.is_err());
        });
    }
}

/// Local browsing preserves the existing LocalFs model; new folder writes use
/// the same rooted admission/current-authority boundary as held upload sources.
pub(super) struct Documents {
    pub local: LocalFs,
    pub sources: Arc<Sources>,
}
impl PaneFs for Documents {
    fn kind(&self) -> PaneKind {
        PaneKind::Local
    }
    fn home(&self) -> String {
        self.local.home()
    }
    fn list(&self, p: &str, o: &ListOptions) -> Result<Listing, SftpError> {
        self.sources.check_root()?;
        let result = self.local.list(p, o);
        self.sources.check_root()?;
        result
    }
    fn stat(&self, p: &str) -> Result<Entry, SftpError> {
        self.sources.check_root()?;
        let result = self.local.stat(p);
        self.sources.check_root()?;
        result
    }
    fn mkdir(&self, p: &str) -> Result<(), SftpError> {
        self.sources.mkdir(p)
    }
    fn rename(&self, _: &str, _: &str) -> Result<(), SftpError> {
        Err(denied())
    }
    fn delete(&self, _: &str, _: bool) -> Result<(), SftpError> {
        Err(denied())
    }
    fn join(&self, p: &str, n: &str) -> Result<String, SftpError> {
        self.local.join(p, n)
    }
    fn parent(&self, p: &str) -> Option<String> {
        self.local.parent(p)
    }
}
