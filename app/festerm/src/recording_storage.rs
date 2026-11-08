use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use festerm_recording::{RecordingSettings, RecordingSettingsStore};

type SettingsResult = (RecordingSettingsStore, Result<RecordingSettings, String>);
type OpenResult = Result<(RecordingStorage, RecordingSettings), String>;

pub(crate) struct OpenTask(Receiver<OpenResult>);

impl OpenTask {
    pub(crate) fn start(wake: impl Fn() + Send + 'static) -> Result<Self, String> {
        let (send, receive) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("festerm-recording-storage".to_owned())
            .spawn(move || {
                let result = open_default();
                let _ = send.send(result);
                wake();
            })
            .map_err(|error| format!("Recording storage worker could not start: {error}"))?;
        Ok(Self(receive))
    }

    pub(crate) fn poll(&self) -> Option<OpenResult> {
        match self.0.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(
                "Recording storage worker stopped unexpectedly".to_owned(),
            )),
        }
    }
}

pub(crate) struct RecordingStorage {
    path: PathBuf,
    store: Option<RecordingSettingsStore>,
    pending: Option<Receiver<SettingsResult>>,
}

impl RecordingStorage {
    #[cfg(test)]
    pub(crate) fn open_for_test(path: &Path) -> io::Result<(Self, RecordingSettings)> {
        open_at(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn save(
        &mut self,
        settings: RecordingSettings,
        wake: impl Fn() + Send + 'static,
    ) -> Result<(), String> {
        self.begin(
            move |store| {
                store.save(&settings).map(|()| settings).map_err(|error| {
                    format!("Recording settings could not be confirmed; reload before retrying: {error}")
                })
            },
            wake,
        )
    }

    pub(crate) fn reload(&mut self, wake: impl Fn() + Send + 'static) -> Result<(), String> {
        self.begin(
            |store| {
                store
                    .reload()
                    .map_err(|error| format!("Recording settings could not be reloaded: {error}"))
            },
            wake,
        )
    }

    fn begin(
        &mut self,
        operation: impl FnOnce(&mut RecordingSettingsStore) -> Result<RecordingSettings, String>
            + Send
            + 'static,
        wake: impl Fn() + Send + 'static,
    ) -> Result<(), String> {
        let mut store = self.store.take().ok_or_else(|| {
            "Recording settings are busy or unavailable; restart the app if this persists"
                .to_owned()
        })?;
        let (send, receive) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("festerm-recording-settings".to_owned())
            .spawn(move || {
                let result = operation(&mut store);
                let _ = send.send((store, result));
                wake();
            })
            .map_err(|error| format!("Recording settings worker could not start: {error}"))?;
        self.pending = Some(receive);
        Ok(())
    }

    pub(crate) fn poll(&mut self) -> Option<Result<RecordingSettings, String>> {
        match self.pending.as_ref()?.try_recv() {
            Ok((store, result)) => {
                self.store = Some(store);
                self.pending = None;
                Some(result)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                Some(Err(
                    "Recording settings worker stopped unexpectedly; restart the app".to_owned(),
                ))
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn test_directory_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        path.to_owned()
    }
    #[cfg(not(windows))]
    {
        path.canonicalize().unwrap()
    }
}

fn open_default() -> OpenResult {
    let path = directories::ProjectDirs::from("com", "fes", "fesTerm")
        .ok_or_else(|| "The device-local data directory is unavailable".to_owned())?
        .data_local_dir()
        .join("terminal-recordings");
    open_at(&path).map_err(|error| format!("Private recording storage is unavailable: {error}"))
}

fn open_at(path: &Path) -> io::Result<(RecordingStorage, RecordingSettings)> {
    let directory = ensure_private_directory(path)?;
    #[cfg(target_os = "macos")]
    festerm_recording_apple::exclude_directory_from_backup(path, &directory)
        .map_err(io::Error::other)?;
    let (store, settings) = RecordingSettingsStore::open(path)?;
    drop(directory);
    Ok((
        RecordingStorage {
            path: path.to_owned(),
            store: Some(store),
            pending: None,
        },
        settings,
    ))
}

fn ensure_private_directory(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "recording storage must be an absolute path",
        ));
    }
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        match fs::symlink_metadata(current) {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(current.to_owned());
                current = current.parent().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "recording storage has no parent",
                    )
                })?;
            }
            Err(error) => return Err(error),
        }
    }
    for path in missing.iter().rev() {
        let directory = festerm_sessiond::private_storage::private_directory(path, true)?;
        sync_parent(path)?;
        drop(directory);
    }
    festerm_sessiond::private_storage::private_directory(path, true)
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    File::open(path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "recording storage has no parent",
        )
    })?)?
    .sync_all()
}

#[cfg(windows)]
fn sync_parent(path: &Path) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "recording storage has no parent",
        )
    })?;
    let directory = festerm_windows_security::lock_directory_path_without_delete_sharing(parent)?;
    festerm_windows_security::lock_directory_for_sync(&directory)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_private_nested_storage_and_reopens_settings() {
        let parent = tempfile::tempdir().unwrap();
        let path = test_directory_path(parent.path())
            .join("app")
            .join("recordings");
        let (mut storage, settings) = open_at(&path).unwrap();
        let changed = RecordingSettings {
            directory_template: "demo-{recording_id}".to_owned(),
            ..settings
        };
        storage.store.as_mut().unwrap().save(&changed).unwrap();
        let (_, reloaded) = open_at(&path).unwrap();
        assert_eq!(reloaded, changed);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_or_nonprivate_recording_directory() {
        use std::{os::unix::fs::symlink, os::unix::fs::PermissionsExt};

        let parent = tempfile::tempdir().unwrap();
        let parent_path = parent.path().canonicalize().unwrap();
        let target = parent_path.join("target");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
        let link = parent_path.join("link");
        symlink(&target, &link).unwrap();
        assert!(open_at(&link).is_err());
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(open_at(&target).is_err());
    }
}
