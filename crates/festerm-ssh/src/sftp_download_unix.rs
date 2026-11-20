//! Ordinary local downloads: retained identity, not private permission repair.
use std::{
    fs::{File, OpenOptions},
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use super::{local_error, SftpSessionError};

type CleanupResult = Result<(), SftpSessionError>;

/// Remains charged to the pending transfer until actual abandoned-creation
/// cleanup finishes. Waiting is bounded by the existing owner shutdown deadline.
#[derive(Default)]
pub(super) struct Completion {
    result: Mutex<Option<CleanupResult>>,
    changed: tokio::sync::Notify,
}
impl Completion {
    fn finish(&self, result: CleanupResult) {
        *self.result.lock().expect("creation completion") = Some(result);
        self.changed.notify_one();
    }
    pub(super) async fn wait(&self) -> CleanupResult {
        loop {
            let notified = self.changed.notified();
            if let Some(result) = self.result.lock().expect("creation completion").clone() {
                return result;
            }
            notified.await;
        }
    }
}

pub(super) struct Binding {
    parent: Option<File>,
    file: File,
    path: PathBuf,
}
impl Binding {
    fn name(&self) -> &Path {
        Path::new(self.path.file_name().expect("created file has a name"))
    }
    fn unconfirmed(&self) -> SftpSessionError {
        SftpSessionError::PartialFileOwnershipUnconfirmed {
            path: self.path.display().to_string(),
        }
    }
    pub(super) fn cleanup(&self) -> CleanupResult {
        let parent = self.parent.as_ref().ok_or_else(|| self.unconfirmed())?;
        // Never unlink through an untrusted parent or a replaced leaf. Missing
        // entries remain uncertain; we do not scan for renamed owned files.
        festerm_unix_security::trusted_private_storage_ancestor(parent)
            .map_err(|_| self.unconfirmed())?;
        if !festerm_unix_security::download_file_matches(parent, self.name(), &self.file)
            .map_err(|_| self.unconfirmed())?
        {
            return Err(self.unconfirmed());
        }
        festerm_unix_security::remove_download_file(parent, self.name(), &self.file)
            .map_err(|error| local_error("remove partial file", &self.path, error))
    }
    /// A negative publication fence only: retained identity cannot authorize a
    /// changed pathname. It does not make subsequent POSIX rename atomic with
    /// respect to hostile same-user/root writers.
    pub(super) fn check_path(&self) -> CleanupResult {
        let Some(parent) = &self.parent else {
            // Unreadable parents preserve existing valid publication behavior;
            // automatic cleanup remains explicitly unconfirmed.
            return Ok(());
        };
        let current = festerm_unix_security::open_download_parent(parent_path(&self.path))
            .and_then(|file| file.metadata())
            .map_err(|_| self.unconfirmed())?;
        let retained = parent.metadata().map_err(|_| self.unconfirmed())?;
        if current.dev() != retained.dev()
            || current.ino() != retained.ino()
            || !festerm_unix_security::download_file_matches(parent, self.name(), &self.file)
                .map_err(|_| self.unconfirmed())?
        {
            return Err(self.unconfirmed());
        }
        Ok(())
    }
}
fn parent_path(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

/// Ownership begins in the blocking creation job, before any fallible clone or
/// async handoff. Dropping an unreceived result schedules, never performs, I/O.
pub(super) struct Creation {
    binding: Option<Arc<Binding>>,
    writer: Option<File>,
    completion: Arc<Completion>,
    runtime: tokio::runtime::Handle,
}
impl Creation {
    pub(super) fn create(
        path: &Path,
        completion: Arc<Completion>,
    ) -> Result<Self, SftpSessionError> {
        let parent = festerm_unix_security::open_download_parent(parent_path(path)).ok();
        // Preserve ordinary open semantics for a trailing slash or dot; Path's
        // normalized file_name alone would turn a directory spelling into a file.
        let name = path.file_name().filter(|name| {
            path.as_os_str()
                .as_bytes()
                .rsplit(|byte| *byte == b'/')
                .next()
                == Some(name.as_bytes())
        });
        let created = match (&parent, name) {
            (Some(parent), Some(name)) => {
                festerm_unix_security::create_download_file(parent, Path::new(name))
            }
            _ => OpenOptions::new().create_new(true).write(true).open(path),
        };
        let file = match created {
            Ok(file) => file,
            Err(error) => {
                completion.finish(Ok(()));
                return Err(local_error("create destination file", path, error));
            }
        };
        let mut guard = Self {
            binding: Some(Arc::new(Binding {
                parent,
                file,
                path: path.to_owned(),
            })),
            writer: None,
            completion,
            runtime: tokio::runtime::Handle::current(),
        };
        match guard
            .binding
            .as_ref()
            .expect("creation binding")
            .file
            .try_clone()
        {
            Ok(writer) => guard.writer = Some(writer),
            Err(error) => {
                let operation_error = local_error("create destination file", path, error);
                let cleanup = guard.binding.take().expect("creation binding").cleanup();
                guard.completion.finish(cleanup.clone());
                return Err(match cleanup {
                    Ok(()) => operation_error,
                    Err(cleanup_error) => SftpSessionError::TransferFailedWithIncompleteCleanup {
                        operation_error: Box::new(operation_error),
                        cleanup_error: Box::new(cleanup_error),
                    },
                });
            }
        }
        Ok(guard)
    }
    pub(super) fn into_files(mut self) -> (File, Arc<Binding>) {
        (
            self.writer.take().expect("creation writer"),
            self.binding.take().expect("creation binding"),
        )
    }
}
impl Drop for Creation {
    fn drop(&mut self) {
        let Some(binding) = self.binding.take() else {
            return;
        };
        drop(self.writer.take());
        let completion = self.completion.clone();
        self.runtime.spawn_blocking(move || {
            let result = binding.cleanup();
            if result.is_err() {
                tracing::warn!(target: "festerm::sftp", "abandoned Unix download creation cleanup incomplete; partial output may remain");
            }
            completion.finish(result);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sftp::{
        begin_transfer_partial, check_transfer_partial_path, cleanup_transfer_partial,
        create_transfer_download_file, forget_transfer_partial, protect_transfer_partial,
    };
    use crate::sftp_transfer::SftpPath;
    use std::{
        fs,
        os::unix::fs::{DirBuilderExt, PermissionsExt},
        sync::atomic::{AtomicU64, Ordering},
    };
    use tokio::io::AsyncWriteExt;

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "festerm-unix-download-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }
        fn file(&self) -> PathBuf {
            self.0.join("owned.part")
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }
    async fn create(path: &Path) -> (Option<super::super::PartialFile>, tokio::fs::File) {
        let mut partial = None;
        begin_transfer_partial(&mut partial, SftpPath::local(path.to_owned())).unwrap();
        let file = create_transfer_download_file(&mut partial, path)
            .await
            .unwrap();
        (partial, file)
    }

    #[test]
    fn unix_download_cleanup_removes_exact_owned_output_and_preserves_sibling() {
        let directory = Directory::new();
        let path = directory.file();
        let sibling = directory.0.join("unrelated.festerm-part");
        fs::write(&sibling, b"unrelated").unwrap();
        runtime().block_on(async {
            let (mut partial, mut file) = create(&path).await;
            file.write_all(b"partial content").await.unwrap();
            file.flush().await.unwrap();
            drop(file);
            cleanup_transfer_partial(&mut partial).await.unwrap();
            assert!(!path.exists());
            assert_eq!(fs::read(&sibling).unwrap(), b"unrelated");
            assert!(partial.is_none());
        });
    }

    #[test]
    fn unix_download_replaced_leaf_is_preserved_and_reported() {
        for symlink in [false, true] {
            let directory = Directory::new();
            let path = directory.file();
            let moved = directory.0.join("moved-owned");
            let target = directory.0.join("foreign");
            fs::write(&target, b"foreign target").unwrap();
            runtime().block_on(async {
                let (mut partial, mut file) = create(&path).await;
                file.write_all(b"owned bytes").await.unwrap();
                file.flush().await.unwrap();
                drop(file);
                fs::rename(&path, &moved).unwrap();
                if symlink {
                    std::os::unix::fs::symlink(&target, &path).unwrap();
                } else {
                    fs::write(&path, b"replacement").unwrap();
                }
                assert!(matches!(
                    cleanup_transfer_partial(&mut partial).await,
                    Err(SftpSessionError::PartialFileOwnershipUnconfirmed { .. })
                ));
                assert_eq!(fs::read(&moved).unwrap(), b"owned bytes");
                assert_eq!(fs::read(&target).unwrap(), b"foreign target");
                assert_eq!(
                    fs::read(&path).unwrap(),
                    if symlink {
                        b"foreign target".as_slice()
                    } else {
                        b"replacement".as_slice()
                    }
                );
            });
        }
    }

    #[test]
    fn unix_download_ancestor_replacement_cleans_retained_parent_only() {
        let directory = Directory::new();
        let selected = directory.0.join("selected");
        let moved = directory.0.join("moved");
        fs::create_dir(&selected).unwrap();
        let path = selected.join("owned.part");
        runtime().block_on(async {
            let (mut partial, mut file) = create(&path).await;
            file.write_all(b"owned bytes").await.unwrap();
            file.flush().await.unwrap();
            drop(file);
            fs::rename(&selected, &moved).unwrap();
            fs::create_dir(&selected).unwrap();
            fs::write(&path, b"replacement").unwrap();
            fs::write(moved.join("sibling"), b"sibling").unwrap();
            assert!(
                check_transfer_partial_path(&partial, &SftpPath::local(path.clone()))
                    .await
                    .is_err()
            );
            cleanup_transfer_partial(&mut partial).await.unwrap();
            assert!(!moved.join("owned.part").exists());
            assert_eq!(fs::read(&path).unwrap(), b"replacement");
            assert_eq!(fs::read(moved.join("sibling")).unwrap(), b"sibling");
        });
    }

    #[test]
    fn unix_download_untrusted_parent_retains_output_without_changing_valid_creation() {
        let directory = Directory::new();
        let path = directory.file();
        runtime().block_on(async {
            // Trust can change after creation; only this owned fixture is changed.
            let (mut partial, mut file) = create(&path).await;
            file.write_all(b"owned bytes").await.unwrap();
            file.flush().await.unwrap();
            drop(file);
            fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o777)).unwrap();
            let cleanup = cleanup_transfer_partial(&mut partial).await;
            fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o700)).unwrap();
            assert!(matches!(
                cleanup,
                Err(SftpSessionError::PartialFileOwnershipUnconfirmed { .. })
            ));
            assert_eq!(fs::read(&path).unwrap(), b"owned bytes");
            // A parent untrusted from the outset still permits an ordinary transfer.
            fs::remove_file(&path).unwrap();
            fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o777)).unwrap();
            let (mut partial, file) = create(&path).await;
            drop(file);
            let cleanup = cleanup_transfer_partial(&mut partial).await;
            fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o700)).unwrap();
            assert!(matches!(
                cleanup,
                Err(SftpSessionError::PartialFileOwnershipUnconfirmed { .. })
            ));
            assert!(path.exists());
        });
    }

    #[test]
    fn unix_download_publication_fence_preserves_temporary_or_published_output() {
        for published in [false, true] {
            let directory = Directory::new();
            let path = directory.file();
            let final_path = directory.0.join("final");
            runtime().block_on(async {
                let (mut partial, mut file) = create(&path).await;
                file.write_all(b"complete").await.unwrap();
                file.flush().await.unwrap();
                drop(file);
                protect_transfer_partial(
                    &mut partial,
                    &SftpPath::local(path.clone()),
                    &SftpPath::local(final_path.clone()),
                );
                if published {
                    fs::rename(&path, &final_path).unwrap();
                }
                assert_eq!(
                    cleanup_transfer_partial(&mut partial).await,
                    Err(SftpSessionError::TransferCommitInterrupted {
                        temporary: path.display().to_string(),
                        destination: final_path.display().to_string()
                    })
                );
                assert_eq!(
                    fs::read(if published { &final_path } else { &path }).unwrap(),
                    b"complete"
                );
            });
        }
    }

    #[test]
    fn unix_download_success_preserves_ordinary_mode_and_security_metadata() {
        let directory = Directory::new();
        let path = directory.file();
        let baseline = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(directory.0.join("ordinary"))
            .unwrap();
        runtime().block_on(async {
            let (mut partial, file) = create(&path).await;
            let expected = festerm_unix_security::security_metadata(&baseline).unwrap();
            let actual =
                festerm_unix_security::security_metadata(&File::open(&path).unwrap()).unwrap();
            assert_eq!(actual, expected);
            drop(file);
            forget_transfer_partial(&mut partial, &SftpPath::local(path.clone()));
            cleanup_transfer_partial(&mut partial).await.unwrap();
            assert!(path.exists());
        });
    }

    #[test]
    fn unix_download_existing_collision_preserves_original_and_finishes_creation_ticket() {
        let directory = Directory::new();
        let path = directory.file();
        fs::write(&path, b"existing").unwrap();
        runtime().block_on(async {
            let completion = Arc::new(Completion::default());
            let returned = tokio::task::spawn_blocking({
                let path = path.clone();
                let completion = completion.clone();
                move || Creation::create(&path, completion)
            })
            .await
            .unwrap();
            assert!(matches!(
                returned,
                Err(SftpSessionError::LocalOperationFailed {
                    operation: "create destination file",
                    ..
                })
            ));
            completion.wait().await.unwrap();
            assert_eq!(fs::read(&path).unwrap(), b"existing");
            for suffix in ["missing/", "missing/."] {
                let invalid = directory.0.join(suffix);
                let completion = Arc::new(Completion::default());
                let returned = tokio::task::spawn_blocking({
                    let completion = completion.clone();
                    move || Creation::create(&invalid, completion)
                })
                .await
                .unwrap();
                assert!(
                    returned.is_err(),
                    "directory spelling must not create a regular file"
                );
                completion.wait().await.unwrap();
                assert!(!directory.0.join("missing").exists());
            }
        });
    }

    #[test]
    fn unix_download_lost_creation_handoff_waits_for_actual_cleanup() {
        let directory = Directory::new();
        let path = directory.file();
        runtime().block_on(async {
            let completion = Arc::new(Completion::default());
            let (created_tx, created_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let job = tokio::task::spawn_blocking({
                let path = path.clone();
                let completion = completion.clone();
                move || {
                    let creation = Creation::create(&path, completion).unwrap();
                    created_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    creation
                }
            });
            created_rx.await.unwrap();
            assert!(path.exists());
            drop(job); // Worker owns creation; its eventual result has no receiver.
            let mut partial = None;
            begin_transfer_partial(&mut partial, SftpPath::local(path.clone())).unwrap();
            partial.as_mut().unwrap().local_creation = Some(completion);
            let mut cleanup = Box::pin(cleanup_transfer_partial(&mut partial));
            let mut context = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(std::future::Future::poll(cleanup.as_mut(), &mut context).is_pending());
            release_tx.send(()).unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(2), cleanup)
                .await
                .unwrap()
                .unwrap();
            assert!(!path.exists());
        });
    }
}
