use std::{
    fs::{self, File},
    io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use festerm_recording::{RecordingSettings, RecordingSettingsStore};

type SettingsResult = (RecordingSettingsStore, Result<RecordingSettings, String>);

/// Private, device-local recording storage. Native hosts establish backup
/// exclusion before calling `open`.
pub(crate) struct RecordingStorage {
    root_path: PathBuf,
    root: File,
    settings: Option<RecordingSettingsStore>,
    pending: Option<Receiver<SettingsResult>>,
}

impl RecordingStorage {
    #[cfg(any(target_os = "ios", target_os = "android"))]
    pub(crate) fn path(&self) -> &Path {
        &self.root_path
    }

    #[cfg(any(target_os = "ios", target_os = "android"))]
    pub(crate) fn open_default() -> Result<(Self, RecordingSettings), String> {
        #[cfg(target_os = "android")]
        let path = festerm_android_platform::recording_directory().map_err(str::to_owned)?;
        #[cfg(target_os = "ios")]
        let path = ios_recording_directory()?;
        Self::open(&path).map_err(|error| format!("Recording storage is unavailable: {error}"))
    }

    #[cfg(any(test, target_os = "ios", target_os = "android"))]
    pub(super) fn open(path: &Path) -> io::Result<(Self, RecordingSettings)> {
        let root = File::open(path)?;
        check_directory(path, &root)?;
        let (settings, values) = RecordingSettingsStore::open(path)?;
        check_directory(path, &root)?;
        Ok((
            Self {
                root_path: path.to_owned(),
                root,
                settings: Some(settings),
                pending: None,
            },
            retain_mobile_source(values),
        ))
    }

    /// Performs disk synchronization away from the session/UI thread.
    pub(crate) fn save(
        &mut self,
        settings: RecordingSettings,
        wake: impl Fn() + Send + 'static,
    ) -> Result<(), String> {
        let settings = retain_mobile_source(settings);
        self.begin(
            move |store| {
                store
                    .save(&settings)
                    .map(|()| settings)
                    .map_err(|error| format!("Recording settings could not be saved: {error}"))
            },
            wake,
        )
    }

    pub(crate) fn reload(&mut self, wake: impl Fn() + Send + 'static) -> Result<(), String> {
        self.begin(
            |store| {
                store
                    .reload()
                    .map(retain_mobile_source)
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
        check_directory(&self.root_path, &self.root)
            .map_err(|error| format!("Recording storage changed: {error}"))?;
        let mut store = self.settings.take().ok_or_else(|| {
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
        let pending = self.pending.as_ref()?;
        match pending.try_recv() {
            Ok((store, result)) => {
                self.settings = Some(store);
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

pub(super) fn retain_mobile_source(mut settings: RecordingSettings) -> RecordingSettings {
    settings.delete_source_after_export = false;
    settings
}

fn check_directory(path: &Path, directory: &File) -> io::Result<()> {
    if !festerm_unix_security::is_current_user_only(directory, true)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "recording storage is not private",
        ));
    }
    let current = fs::symlink_metadata(path)?;
    let pinned = directory.metadata()?;
    if !current.is_dir() || current.dev() != pinned.dev() || current.ino() != pinned.ino() {
        return Err(io::Error::other("recording storage directory changed"));
    }
    Ok(())
}

#[cfg(target_os = "ios")]
fn ios_recording_directory() -> Result<PathBuf, String> {
    let parent_path = crate::Profiles::default_directory()
        .ok_or_else(|| "Recording storage is unavailable".to_owned())?;
    let parent = File::open(&parent_path)
        .map_err(|error| format!("Recording storage is unavailable: {error}"))?;
    let current = fs::symlink_metadata(&parent_path)
        .map_err(|error| format!("Recording storage is unavailable: {error}"))?;
    let pinned = parent
        .metadata()
        .map_err(|error| format!("Recording storage is unavailable: {error}"))?;
    if !current.is_dir() || current.dev() != pinned.dev() || current.ino() != pinned.ino() {
        return Err("The app storage directory changed".to_owned());
    }
    festerm_unix_security::trusted_private_storage_ancestor(&parent)
        .map_err(|error| format!("Recording storage is unavailable: {error}"))?;
    let name = Path::new("terminal-recordings");
    let root = match festerm_unix_security::open_directory_read_nofollow(&parent, name) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match festerm_unix_security::create_private_directory(&parent, name) {
                Ok(root) => {
                    parent.sync_all().map_err(|error| {
                        format!("Recording storage could not be synced: {error}")
                    })?;
                    root
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    festerm_unix_security::open_directory_read_nofollow(&parent, name)
                        .map_err(|error| format!("Recording storage is unavailable: {error}"))?
                }
                Err(error) => return Err(format!("Recording storage is unavailable: {error}")),
            }
        }
        Err(error) => return Err(format!("Recording storage is unavailable: {error}")),
    };
    let path = parent_path.join(name);
    check_directory(&path, &root)
        .map_err(|error| format!("Recording storage is unavailable: {error}"))?;
    festerm_recording_apple::exclude_directory_from_backup(&path, &root)
        .map_err(|error| format!("Recording storage cannot be excluded from backup: {error}"))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn legacy_deletion_preference_is_not_enabled_on_mobile() {
        let root = tempfile::tempdir().unwrap();
        festerm_unix_security::make_private_directory(&File::open(root.path()).unwrap()).unwrap();
        let (mut store, mut legacy) = RecordingSettingsStore::open(root.path()).unwrap();
        legacy.delete_source_after_export = true;
        store.save(&legacy).unwrap();
        drop(store);

        let (mut storage, settings) = RecordingStorage::open(root.path()).unwrap();
        assert!(!settings.delete_source_after_export);
        let (wake, ready) = mpsc::channel();
        storage.reload(move || wake.send(()).unwrap()).unwrap();
        ready.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(!storage.poll().unwrap().unwrap().delete_source_after_export);
    }

    #[test]
    fn saves_settings_off_thread_and_reloads_them() {
        let root = tempfile::tempdir().unwrap();
        festerm_unix_security::make_private_directory(&File::open(root.path()).unwrap()).unwrap();
        let (mut storage, settings) = RecordingStorage::open(root.path()).unwrap();
        let changed = RecordingSettings {
            normalize_typing: true,
            delete_source_after_export: true,
            ..settings
        };
        let expected = retain_mobile_source(changed.clone());
        let (wake, ready) = mpsc::channel();
        storage
            .save(changed.clone(), move || wake.send(()).unwrap())
            .unwrap();
        ready.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(storage.poll().unwrap().unwrap(), expected);
        let (wake, ready) = mpsc::channel();
        storage.reload(move || wake.send(()).unwrap()).unwrap();
        ready.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(storage.poll().unwrap().unwrap(), expected);
    }
}
