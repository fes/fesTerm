//! Reuse the retained-directory/file primitives used by ordinary SFTP downloads.
//! Identity checks are negative fences, not immunity to hostile same-user races.
use super::local_root::Root;
#[cfg(unix)]
use festerm_sftp::path;
use festerm_sftp::SftpError;
use std::sync::Arc;
#[cfg(unix)]
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
#[cfg(unix)]
static NEXT: AtomicU64 = AtomicU64::new(1);
fn uncertain() -> SftpError {
    SftpError::Io("Download ownership could not be confirmed; no replacement was removed".into())
}
#[cfg(unix)]
pub(super) struct Stage {
    root: Arc<Root>,
    components: Vec<String>,
    parent: File,
    file: File,
    name: PathBuf,
    root_file: File,
    published: bool,
}
#[cfg(unix)]
impl Stage {
    pub(super) fn create(local: &Arc<Root>, directory: &str) -> Result<Self, SftpError> {
        let components = path::local_components(directory)?
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let root = local.clone();
        let root_file = root.directory().map_err(|_| uncertain())?;
        let mut parent = root_file.try_clone().map_err(|_| uncertain())?;
        for name in &components {
            parent = festerm_unix_security::open_directory_read_nofollow(&parent, Path::new(name))
                .map_err(|_| uncertain())?;
        }
        festerm_unix_security::trusted_private_storage_ancestor(&parent)
            .map_err(|_| uncertain())?;
        for _ in 0..8 {
            let mut current = NEXT.load(Ordering::Relaxed);
            let id = loop {
                let next = current.checked_add(1).ok_or_else(uncertain)?;
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
            let name = PathBuf::from(format!(
                ".festerm-download-{}-{id}.part",
                std::process::id()
            ));
            match festerm_unix_security::create_download_file(&parent, &name) {
                Ok(file) => {
                    return Ok(Self {
                        root,
                        components,
                        parent,
                        file,
                        name,
                        root_file,
                        published: false,
                    })
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(uncertain()),
            }
        }
        Err(uncertain())
    }
    pub(super) fn writer(&self) -> Result<tokio::fs::File, SftpError> {
        self.file
            .try_clone()
            .map(tokio::fs::File::from_std)
            .map_err(|_| uncertain())
    }
    fn check(&self) -> Result<(), SftpError> {
        use std::os::unix::fs::MetadataExt;
        fn same(a: &File, b: &File) -> Result<bool, SftpError> {
            let a = a.metadata().map_err(|_| uncertain())?;
            let b = b.metadata().map_err(|_| uncertain())?;
            Ok(a.dev() == b.dev() && a.ino() == b.ino())
        }
        let mut current = self.root.directory().map_err(|_| uncertain())?;
        if !same(&current, &self.root_file)? {
            return Err(uncertain());
        }
        for name in &self.components {
            current =
                festerm_unix_security::open_directory_read_nofollow(&current, Path::new(name))
                    .map_err(|_| uncertain())?;
        }
        if !same(&current, &self.parent)?
            || !festerm_unix_security::download_file_matches(&self.parent, &self.name, &self.file)
                .map_err(|_| uncertain())?
        {
            return Err(uncertain());
        }
        Ok(())
    }
    pub(super) fn publish(
        &mut self,
        name: &str,
        authorize: impl FnOnce() -> Result<(), SftpError>,
    ) -> Result<(), SftpError> {
        path::validate_name(name)?;
        self.file
            .sync_all()
            .map_err(|_| SftpError::Io("Download could not be finished".into()))?;
        self.check()?;
        authorize()?;
        festerm_unix_security::trusted_private_storage_ancestor(&self.parent)
            .map_err(|_| uncertain())?;
        festerm_unix_security::rename_noreplace(
            &self.parent,
            &self.name,
            &self.parent,
            Path::new(name),
        )
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                SftpError::AlreadyExists(
                    "Choose a different destination; existing files are preserved".into(),
                )
            } else {
                uncertain()
            }
        })?;
        self.published = true;
        Ok(())
    }
    pub(super) fn cleanup(&mut self) -> Result<(), SftpError> {
        if self.published {
            return Ok(());
        }
        festerm_unix_security::trusted_private_storage_ancestor(&self.parent)
            .map_err(|_| uncertain())?;
        festerm_unix_security::remove_download_file(&self.parent, &self.name, &self.file)
            .map_err(|_| uncertain())?;
        self.published = true;
        Ok(())
    }
}
#[cfg(unix)]
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}
#[cfg(not(unix))]
pub(super) struct Stage;
#[cfg(not(unix))]
impl Stage {
    pub(super) fn create(_: &Arc<Root>, _: &str) -> Result<Self, SftpError> {
        Err(uncertain())
    }
    pub(super) fn writer(&self) -> Result<tokio::fs::File, SftpError> {
        Err(uncertain())
    }
    pub(super) fn publish(
        &mut self,
        _: &str,
        _: impl FnOnce() -> Result<(), SftpError>,
    ) -> Result<(), SftpError> {
        Err(uncertain())
    }
    pub(super) fn cleanup(&mut self) -> Result<(), SftpError> {
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{fs, io::Write};
    fn setup() -> (tempfile::TempDir, Arc<Root>) {
        let root = tempfile::tempdir().unwrap();
        let local = Root::open(root.path()).unwrap();
        (root, local)
    }
    #[test]
    fn download_stage_rejects_canonical_root_replaced_with_symlink_before_create() {
        use std::os::unix::fs::symlink;
        let base = tempfile::tempdir().unwrap();
        let documents = base.path().join("Documents");
        let outside = base.path().join("outside");
        fs::create_dir(&documents).unwrap();
        fs::create_dir(&outside).unwrap();
        let local = Root::open(&documents).unwrap();
        fs::rename(&documents, base.path().join("original-Documents")).unwrap();
        symlink(&outside, &documents).unwrap();
        assert!(
            Stage::create(&local, "").is_err(),
            "a replaced canonical root is not a new admission baseline"
        );
        assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
    }
    #[test]
    fn download_stage_refuses_parent_that_becomes_foreign_writable_before_publish() {
        use std::os::unix::fs::PermissionsExt;
        let (root, local) = setup();
        let original = fs::metadata(root.path()).unwrap().permissions();
        let mut stage = Stage::create(&local, "").unwrap();
        stage.file.write_all(b"public download").unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o777)).unwrap();
        let result = stage.publish("final", || Ok(()));
        fs::set_permissions(root.path(), original).unwrap();
        assert!(
            result.is_err(),
            "unchanged identity cannot authorize a newly writable parent"
        );
        assert!(!root.path().join("final").exists());
        stage.cleanup().unwrap();
    }

    #[test]
    fn download_stage_never_removes_a_replacement_partial() {
        let (root, local) = setup();
        let mut stage = Stage::create(&local, "").unwrap();
        let name = stage.name.clone();
        fs::rename(
            root.path().join(&name),
            root.path().join("retained-original"),
        )
        .unwrap();
        fs::write(root.path().join(&name), b"replacement").unwrap();
        assert!(stage.cleanup().is_err());
        assert!(stage.publish("final", || Ok(())).is_err());
        drop(stage);
        assert_eq!(fs::read(root.path().join(name)).unwrap(), b"replacement");
        assert!(root.path().join("retained-original").exists());
        assert!(!root.path().join("final").exists());
    }
    #[test]
    fn download_stage_parent_replacement_refuses_publication_and_cleans_only_retained_file() {
        let (root, local) = setup();
        fs::create_dir(root.path().join("target")).unwrap();
        let mut stage = Stage::create(&local, "target").unwrap();
        let name = stage.name.clone();
        fs::rename(root.path().join("target"), root.path().join("old-target")).unwrap();
        fs::create_dir(root.path().join("target")).unwrap();
        fs::write(root.path().join("target").join(&name), b"replacement").unwrap();
        assert!(stage.publish("final", || Ok(())).is_err());
        stage.cleanup().unwrap();
        assert_eq!(
            fs::read(root.path().join("target").join(&name)).unwrap(),
            b"replacement"
        );
        assert!(!root.path().join("old-target").join(name).exists());
        assert!(!root.path().join("target/final").exists());
    }
    #[test]
    fn download_stage_no_clobber_and_authority_cutover_preserve_originals() {
        let (root, local) = setup();
        fs::write(root.path().join("final"), b"original").unwrap();
        let mut stage = Stage::create(&local, "").unwrap();
        stage.file.write_all(b"public download").unwrap();
        assert!(stage.publish("final", || Ok(())).is_err());
        stage.cleanup().unwrap();
        assert_eq!(fs::read(root.path().join("final")).unwrap(), b"original");
        let mut stage = Stage::create(&local, "").unwrap();
        assert!(stage.publish("new", || Err(SftpError::Cancelled)).is_err());
        stage.cleanup().unwrap();
        assert!(!root.path().join("new").exists());
        let mut stage = Stage::create(&local, "").unwrap();
        stage.file.write_all(b"public download").unwrap();
        stage.publish("new", || Ok(())).unwrap();
        assert_eq!(
            fs::read(root.path().join("new")).unwrap(),
            b"public download"
        );
    }
}
