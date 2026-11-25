//! One startup Documents identity, retained before LocalFs canonicalization.
use super::*;
use std::path::{Path, PathBuf};
fn refused() -> SftpError {
    SftpError::Io("App Documents changed or could not be opened safely".into())
}
#[cfg(unix)]
pub(super) struct Root {
    path: PathBuf,
    file: std::fs::File,
    preview: Mutex<Option<(String, std::fs::File)>>,
}
#[cfg(unix)]
impl Root {
    pub(super) fn open(selected: &Path) -> Result<Arc<Self>, SftpError> {
        // Apple's documented /var alias is the only normalized prefix. Never
        // canonicalize a Documents leaf or an arbitrary selected ancestor.
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        let path = if let Ok(rest) = selected.strip_prefix("/var") {
            Path::new("/private/var").join(rest)
        } else {
            selected.to_owned()
        };
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        let path = selected.to_owned();
        let file = festerm_unix_security::open_absolute_directory_read_nofollow(&path)
            .map_err(|_| refused())?;
        festerm_unix_security::trusted_private_storage_ancestor(&file).map_err(|_| refused())?;
        let root = Arc::new(Self {
            path,
            file,
            preview: Mutex::new(None),
        });
        root.check()?;
        Ok(root)
    }
    pub(super) fn root(&self) -> &Path {
        &self.path
    }
    pub(super) fn directory(&self) -> Result<std::fs::File, SftpError> {
        self.check()?;
        self.file.try_clone().map_err(|_| refused())
    }
    pub(super) fn check(&self) -> Result<(), SftpError> {
        use std::os::unix::fs::MetadataExt;
        let current = festerm_unix_security::open_absolute_directory_read_nofollow(&self.path)
            .map_err(|_| refused())?;
        let actual = current.metadata().map_err(|_| refused())?;
        let original = self.file.metadata().map_err(|_| refused())?;
        if actual.dev() != original.dev() || actual.ino() != original.ino() {
            return Err(refused());
        }
        festerm_unix_security::trusted_private_storage_ancestor(&current).map_err(|_| refused())
    }
    /// Creates only a fresh private per-session namespace; never adopts an
    /// existing `.festerm-cache` or reclaims arbitrary directory contents.
    pub(super) fn prepare_preview(&self) -> Result<String, SftpError> {
        use std::os::unix::fs::MetadataExt;
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = self.directory()?;
        let mut owned = self.preview.lock().unwrap();
        if let Some((name, expected)) = owned.as_ref() {
            let current =
                festerm_unix_security::open_directory_read_nofollow(&root, Path::new(name))
                    .map_err(|_| refused())?;
            let actual = current.metadata().map_err(|_| refused())?;
            let expected = expected.metadata().map_err(|_| refused())?;
            if actual.dev() != expected.dev() || actual.ino() != expected.ino() {
                return Err(refused());
            }
            self.check()?;
            return Ok(name.clone());
        }
        for _ in 0..8 {
            let mut current = NEXT.load(Ordering::Relaxed);
            let id = loop {
                let next = current.checked_add(1).ok_or_else(refused)?;
                match NEXT.compare_exchange_weak(
                    current,
                    next,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break current,
                    Err(actual) => current = actual,
                }
            };
            let name = format!(".festerm-preview-{}-{id}", std::process::id());
            match festerm_unix_security::create_private_directory(&root, Path::new(&name)) {
                Ok(file) => {
                    *owned = Some((name.clone(), file));
                    self.check()?;
                    return Ok(name);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(refused()),
            }
        }
        Err(refused())
    }
    /// Reads one regular no-follow local leaf through the pinned root with an
    /// actual cap+1 read bound. The SSH compatibility path is not changed.
    pub(super) fn read_text(&self, relative: &str, limit: usize) -> Result<String, String> {
        use std::{io::Read, os::unix::fs::MetadataExt};
        let fail = || "Could not read the document safely".to_owned();
        let mut components = path::local_components(relative).map_err(|_| fail())?;
        let name = components.pop().ok_or_else(fail)?;
        let mut parent = self.directory().map_err(|_| fail())?;
        for part in components {
            parent = festerm_unix_security::open_directory_read_nofollow(&parent, Path::new(part))
                .map_err(|_| fail())?;
            festerm_unix_security::trusted_private_storage_ancestor(&parent).map_err(|_| fail())?;
        }
        let file = festerm_unix_security::open_file_nofollow(&parent, Path::new(name))
            .map_err(|_| fail())?;
        let before = file.metadata().map_err(|_| fail())?;
        let cap = u64::try_from(limit)
            .ok()
            .and_then(|n| n.checked_add(1))
            .ok_or_else(fail)?;
        if before.len() > limit as u64 {
            return Err("Documents over 4 MB cannot be previewed".into());
        }
        let mut bytes = Vec::new();
        (&file)
            .take(cap)
            .read_to_end(&mut bytes)
            .map_err(|_| fail())?;
        if bytes.len() > limit {
            return Err("Documents over 4 MB cannot be previewed".into());
        }
        self.check().map_err(|_| fail())?;
        let after = file.metadata().map_err(|_| fail())?;
        if before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
            || !festerm_unix_security::download_file_matches(&parent, Path::new(name), &file)
                .map_err(|_| fail())?
        {
            return Err(fail());
        }
        String::from_utf8(bytes).map_err(|_| "The document is not UTF-8 text".into())
    }
    pub(super) fn local_fs(&self) -> Result<LocalFs, SftpError> {
        self.check()?;
        let local = LocalFs::new(&self.path)?;
        if local.root() != self.path {
            return Err(refused());
        }
        self.check()?;
        Ok(local)
    }
}
#[cfg(not(unix))]
pub(super) struct Root;
#[cfg(not(unix))]
impl Root {
    pub(super) fn open(_: &Path) -> Result<Arc<Self>, SftpError> {
        Err(refused())
    }
    pub(super) fn check(&self) -> Result<(), SftpError> {
        Err(refused())
    }
    pub(super) fn prepare_preview(&self) -> Result<String, SftpError> {
        Err(refused())
    }
    pub(super) fn read_text(&self, _: &str, _: usize) -> Result<String, String> {
        Err("Local preview is unavailable".into())
    }

    pub(super) fn local_fs(&self) -> Result<LocalFs, SftpError> {
        Err(refused())
    }
}
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn host_files_documents_startup_rejects_existing_alias_before_canonicalization() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("outside")).unwrap();
        let selected = root.path().join("Documents");
        std::os::unix::fs::symlink(root.path().join("outside"), &selected).unwrap();
        assert!(Root::open(&selected).is_err());
    }
    #[test]
    fn host_files_local_preview_requires_regular_nofollow_leaf_and_bounded_bytes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("public.md"), b"public\n").unwrap();
        std::fs::create_dir(dir.path().join("folder")).unwrap();
        std::os::unix::fs::symlink("public.md", dir.path().join("alias.md")).unwrap();
        let root = Root::open(dir.path()).unwrap();
        assert_eq!(root.read_text("public.md", 7).unwrap(), "public\n");
        assert!(root.read_text("public.md", 6).is_err());
        assert!(root.read_text("folder", 7).is_err());
        assert!(root.read_text("alias.md", 7).is_err());
        assert!(root.read_text("../outside.md", 7).is_err());
        let first = root.prepare_preview().unwrap();
        assert_eq!(root.prepare_preview().unwrap(), first);
        let second_session = Root::open(dir.path()).unwrap();
        assert_ne!(second_session.prepare_preview().unwrap(), first);
    }
    #[test]
    fn host_files_documents_startup_pin_refuses_regular_root_replacement_before_use() {
        let root = tempfile::tempdir().unwrap();
        let selected = root.path().join("Documents");
        std::fs::create_dir(&selected).unwrap();
        let bound = Root::open(&selected).unwrap();
        let sources = local_upload::Sources::new(bound.clone(), Arc::new(|| Ok(())));
        std::fs::rename(&selected, root.path().join("retained")).unwrap();
        std::fs::create_dir(&selected).unwrap();
        assert!(bound.local_fs().is_err());
        assert!(bound.directory().is_err());
        std::fs::write(selected.join("public.txt"), b"replacement root bytes").unwrap();
        assert!(sources.admit("public.txt").is_err());
        assert!(sources.mkdir("new-folder").is_err());
        assert!(local_download::Stage::create(&bound, "").is_err());
        assert_eq!(std::fs::read_dir(&selected).unwrap().count(), 1);
    }
}
