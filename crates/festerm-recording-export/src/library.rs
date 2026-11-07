use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
        Arc,
    },
    thread,
};

use crate::PrivateStaging;
#[cfg(target_os = "android")]
use crate::ProviderJournal;
use festerm_recording::{RecordingId, RecordingLease, RecordingManifest, RecordingSource};

type Wake = Arc<dyn Fn() + Send + Sync>;

pub struct LibrarySnapshot {
    pub sources: Vec<RecordingSource>,
    pub manifests: Vec<RecordingManifest>,
    pub staging: Vec<PrivateStaging>,
    #[cfg(target_os = "android")]
    pub journals: Vec<ProviderJournal>,
}

pub enum LibraryResult {
    Discovered(bool),
    Snapshot(LibrarySnapshot),
    Changed(String),
}

pub struct LibraryTask {
    receiver: Receiver<Result<LibraryResult, String>>,
    cancelled: Arc<AtomicBool>,
}

impl LibraryTask {
    fn start(
        name: &str,
        operation: impl FnOnce(&AtomicBool) -> Result<LibraryResult, String> + Send + 'static,
        wake: Wake,
    ) -> Result<Self, String> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let (send, receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let result = operation(&worker_cancelled);
                let _ = send.send(result);
                wake();
            })
            .map_err(|error| format!("Recording library worker could not start: {error}"))?;
        Ok(Self {
            receiver,
            cancelled,
        })
    }

    pub fn discover(root: PathBuf, wake: Wake) -> Result<Self, String> {
        Self::start(
            "festerm-recording-discovery",
            move |cancelled| {
                let lease = acquire(&root)?;
                let mut found = false;
                for entry in fs::read_dir(&root)
                    .map_err(|error| format!("Could not inspect recording storage: {error}"))?
                {
                    if cancelled.load(Ordering::Acquire) {
                        return Err("Recording library inspection was cancelled".to_owned());
                    }
                    let entry = entry
                        .map_err(|error| format!("Could not inspect recording storage: {error}"))?;
                    let name = entry.file_name();
                    let Some(name) = name.to_str() else {
                        continue;
                    };
                    found |= name.starts_with("recording-meta-")
                        || name.starts_with("recording-stage-")
                        || name.starts_with("provider-")
                        || (name.starts_with("recording-")
                            && (name.ends_with(".fstrec") || name.ends_with(".partial")));
                }
                lease
                    .verify_for(&root)
                    .map_err(|error| format!("Recording storage changed: {error}"))?;
                Ok(LibraryResult::Discovered(found))
            },
            wake,
        )
    }

    pub fn scan(root: PathBuf, wake: Wake) -> Result<Self, String> {
        Self::start(
            "festerm-recording-library",
            move |cancelled| {
                let lease = acquire(&root)?;
                let sources = RecordingSource::recover_with_cancellation(&root, &lease, cancelled)
                    .map_err(|error| format!("Could not inspect recording sources: {error}"))?;
                let manifests = RecordingManifest::recover(&root)
                    .map_err(|error| format!("Could not inspect recording options: {error}"))?;
                let staging = PrivateStaging::recover(&root, &lease)
                    .map_err(|error| format!("Could not inspect private video staging: {error}"))?;
                #[cfg(target_os = "android")]
                let journals = ProviderJournal::recover(&root)
                    .map_err(|error| format!("Could not inspect interrupted exports: {error}"))?;
                if cancelled.load(Ordering::Acquire) {
                    return Err("Recording library inspection was cancelled".to_owned());
                }
                lease
                    .verify_for(&root)
                    .map_err(|error| format!("Recording storage changed: {error}"))?;
                Ok(LibraryResult::Snapshot(LibrarySnapshot {
                    sources,
                    manifests,
                    staging,
                    #[cfg(target_os = "android")]
                    journals,
                }))
            },
            wake,
        )
    }

    pub fn delete_source(
        root: PathBuf,
        source: RecordingSource,
        manifest: Option<RecordingManifest>,
        wake: Wake,
    ) -> Result<Self, String> {
        Self::start(
            "festerm-recording-delete",
            move |cancelled| {
                let lease = acquire(&root)?;
                let id = source.id();
                if let Some(manifest) = &manifest {
                    if manifest.id() != id {
                        return Err("Recording source and options do not match".to_owned());
                    }
                    manifest
                        .verify(&lease)
                        .map_err(|error| format!("Recording options changed: {error}"))?;
                }
                if cancelled.load(Ordering::Acquire) {
                    return Err("Recording deletion was cancelled".to_owned());
                }
                source
                    .delete(&lease)
                    .map_err(|error| {
                        format!("Recording source deletion could not be completed; refresh the library: {error}")
                    })?;
                if let Some(manifest) = manifest {
                    let remains = source_exists(&root, id, &lease).map_err(|error| {
                        format!("The source was deleted, but remaining sources could not be checked: {error}")
                    })?;
                    if !remains {
                        manifest.verify(&lease).map_err(|error| {
                            format!(
                                "The source was deleted, but its saved options changed: {error}"
                            )
                        })?;
                        manifest.remove().map_err(|error| {
                            format!("The source was deleted, but removing its saved options could not be completed; refresh the library: {error}")
                        })?;
                    }
                }
                Ok(LibraryResult::Changed(
                    "Recording source deleted.".to_owned(),
                ))
            },
            wake,
        )
    }

    pub fn delete_manifest(
        root: PathBuf,
        manifest: RecordingManifest,
        wake: Wake,
    ) -> Result<Self, String> {
        Self::start(
            "festerm-recording-delete-options",
            move |cancelled| {
                let lease = acquire(&root)?;
                manifest
                    .verify(&lease)
                    .map_err(|error| format!("Recording options changed: {error}"))?;
                if source_exists(&root, manifest.id(), &lease)
                    .map_err(|error| format!("Could not check recording sources: {error}"))?
                {
                    return Err(
                        "A recording source still uses these options; refresh the library".into(),
                    );
                }
                if cancelled.load(Ordering::Acquire) {
                    return Err("Recording deletion was cancelled".to_owned());
                }
                manifest
                    .verify(&lease)
                    .map_err(|error| format!("Recording options changed: {error}"))?;
                manifest
                    .remove()
                    .map_err(|error| format!("Removing recording options could not be completed; refresh the library: {error}"))?;
                Ok(LibraryResult::Changed(
                    "Unfinished recording options deleted.".to_owned(),
                ))
            },
            wake,
        )
    }

    pub fn delete_staging(
        root: PathBuf,
        staging: PrivateStaging,
        wake: Wake,
    ) -> Result<Self, String> {
        Self::start(
            "festerm-recording-delete-staging",
            move |cancelled| {
                let lease = acquire(&root)?;
                if cancelled.load(Ordering::Acquire) {
                    return Err("Video staging cleanup was cancelled".to_owned());
                }
                lease
                    .verify_for(&root)
                    .map_err(|error| format!("Recording storage changed: {error}"))?;
                staging
                    .cleanup()
                    .map_err(|error| format!("Private video staging cleanup was incomplete; refresh the library: {error}"))?;
                Ok(LibraryResult::Changed(
                    "Interrupted private video staging deleted.".to_owned(),
                ))
            },
            wake,
        )
    }

    #[cfg(target_os = "android")]
    pub fn acknowledge_journal(
        root: PathBuf,
        journal: ProviderJournal,
        wake: Wake,
    ) -> Result<Self, String> {
        Self::start(
            "festerm-recording-acknowledge",
            move |cancelled| {
                let lease = acquire(&root)?;
                if cancelled.load(Ordering::Acquire) {
                    return Err("Export acknowledgement was cancelled".to_owned());
                }
                lease
                    .verify_for(&root)
                    .map_err(|error| format!("Recording storage changed: {error}"))?;
                journal.remove().map_err(|error| {
                    format!("Export acknowledgement could not be completed; refresh the library: {error}")
                })?;
                Ok(LibraryResult::Changed(
                    "Export notice acknowledged; the destination was not changed.".to_owned(),
                ))
            },
            wake,
        )
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn poll(&self) -> Option<Result<LibraryResult, String>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(
                "Recording library worker stopped unexpectedly; refresh the library to check the outcome"
                    .to_owned()
            )),
        }
    }
}

impl Drop for LibraryTask {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn acquire(root: &Path) -> Result<RecordingLease, String> {
    RecordingLease::acquire(root)
        .map_err(|error| format!("Recording storage is unavailable: {error}"))
}

fn source_exists(root: &Path, id: RecordingId, lease: &RecordingLease) -> std::io::Result<bool> {
    lease.verify_for(root)?;
    let mut found = false;
    for extension in ["fstrec", "partial"] {
        match fs::symlink_metadata(root.join(format!("recording-{id}.{extension}"))) {
            Ok(_) => found = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    lease.verify_for(root)?;
    Ok(found)
}
