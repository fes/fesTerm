//! Exact ownership for a local download before publication. Ordinary download
//! permissions still inherit from the selected parent; this is not a vault.
use super::{local_error, SftpSessionError};
use std::{fs::File, os::windows::fs::OpenOptionsExt, path::Path};
use windows_sys::Win32::{Foundation::GENERIC_WRITE, Storage::FileSystem::DELETE};

pub(super) struct Creation {
    file: Option<File>,
    writer: Option<File>,
    runtime: tokio::runtime::Handle,
    #[cfg(test)]
    cleanup_done: Option<std::sync::mpsc::Sender<()>>,
}

impl Creation {
    // Called inside spawn_blocking: the guard exists before any fallible clone
    // or asynchronous handoff. Dropping an undelivered result removes only the
    // exact file created here, even if its pathname has since been replaced.
    pub(super) fn create(path: &Path) -> Result<Self, SftpSessionError> {
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .access_mode(GENERIC_WRITE | DELETE)
            .open(path)
            .map_err(|error| local_error("create destination file", path, error))?;
        let mut creation = Self {
            file: Some(file),
            writer: None,
            runtime: tokio::runtime::Handle::current(),
            #[cfg(test)]
            cleanup_done: None,
        };
        match creation
            .file
            .as_ref()
            .expect("created download")
            .try_clone()
        {
            Ok(writer) => creation.writer = Some(writer),
            Err(error) => {
                let operation_error = local_error("retain destination file", path, error);
                let file = creation.file.take().expect("created download");
                return match festerm_windows_security::remove_private_staging_file(&file) {
                    Ok(()) => Err(operation_error),
                    Err(error) => Err(SftpSessionError::TransferFailedWithIncompleteCleanup {
                        operation_error: Box::new(operation_error),
                        cleanup_error: Box::new(local_error("remove partial file", path, error)),
                    }),
                };
            }
        }
        Ok(creation)
    }

    pub(super) fn into_files(mut self) -> (File, File) {
        (
            self.writer.take().expect("download writer"),
            self.file.take().expect("retained download"),
        )
    }
}

impl Drop for Creation {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            drop(self.writer.take());
            #[cfg(test)]
            let cleanup_done = self.cleanup_done.take();
            // A completed blocking result may be abandoned on the async thread.
            // Never run a potentially stalled filesystem syscall in that Drop.
            // As with normal teardown, runtime/process termination can interrupt
            // this best-effort cleanup; the pending creation remains reportable.
            drop(self.runtime.spawn_blocking(move || {
                if festerm_windows_security::remove_private_staging_file(&file).is_err() {
                    tracing::warn!(target: "festerm::sftp", "abandoned local download creation could not be removed");
                }
                drop(file);
                #[cfg(test)]
                if let Some(done) = cleanup_done { let _ = done.send(()); }
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_download_abandoned_creation_handoff_removes_owned_output() {
        let root =
            std::env::temp_dir().join(format!("festerm-download-handoff-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("part.bin");
        // Exclusive creation is the ownership proof; never clear a pre-existing
        // fixture by suffix. A leftover from an interrupted test is a failure.
        assert!(!path.exists());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (created, creation_ready) = std::sync::mpsc::channel();
            let (release, released) = std::sync::mpsc::channel();
            let (dropped, destruction_done) = std::sync::mpsc::channel();
            let owned_path = path.clone();
            let handoff = tokio::task::spawn_blocking(move || {
                let mut creation = Creation::create(&owned_path).unwrap();
                creation.cleanup_done = Some(dropped);
                created.send(()).unwrap();
                released.recv().unwrap();
                creation
            });
            creation_ready
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap();
            assert!(path.exists());
            drop(handoff); // the awaiting transfer has been canceled
            release.send(()).unwrap();
            destruction_done
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap();
            assert!(!path.exists());
        });
        std::fs::remove_dir(root).unwrap();
    }
}
