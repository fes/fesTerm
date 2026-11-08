#[cfg(unix)]
use std::fs;
use std::{
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::RecordingSettings;

mod catalog;
mod lease;
mod sources;

pub use catalog::RecordingManifest;
pub use lease::RecordingLease;
pub use sources::{RecordingSource, RecordingSourceState};

const VERSION: u8 = 1;
const SETTINGS_NAME: &str = "recording-settings.json";
const LOCK_NAME: &str = "recording-settings.lock";
const MAX_SETTINGS_BYTES: u64 = 512 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SettingsFile {
    version: u8,
    settings: RecordingSettings,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Snapshot {
    identity: (u64, u64),
    digest: [u8; 32],
}

/// Device-local recording preferences in an existing private directory.
///
/// The host must establish the platform's backup policy before opening the
/// store. A save refuses to overwrite a file changed since the last load.
pub struct RecordingSettingsStore {
    directory_path: PathBuf,
    directory: File,
    snapshot: Option<Snapshot>,
}

impl RecordingSettingsStore {
    pub fn open(directory_path: &Path) -> io::Result<(Self, RecordingSettings)> {
        let directory = platform::open_directory(directory_path)?;
        let mut store = Self {
            directory_path: directory_path.to_owned(),
            directory,
            snapshot: None,
        };
        let settings = store.reload()?;
        Ok((store, settings))
    }

    pub fn reload(&mut self) -> io::Result<RecordingSettings> {
        self.check_directory()?;
        let _lock = self.lock()?;
        let (snapshot, settings) = self.read_current()?;
        self.check_directory()?;
        self.snapshot = snapshot;
        Ok(settings)
    }

    pub fn save(&mut self, settings: &RecordingSettings) -> io::Result<()> {
        settings
            .validate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let bytes = serde_json::to_vec(&SettingsFile {
            version: VERSION,
            settings: settings.clone(),
        })
        .map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_SETTINGS_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "recording settings are too large",
            ));
        }
        self.check_directory()?;
        let _lock = self.lock()?;
        let (current, _) = self.read_current()?;
        if current != self.snapshot {
            return Err(io::Error::other(
                "recording settings changed elsewhere; reload before saving",
            ));
        }
        let name = format!(".recording-settings-{}.tmp", Uuid::new_v4().simple());
        let mut file = platform::create_file(&self.directory, Path::new(&name))?;
        let mut published = false;
        let result = (|| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            self.check_directory()?;
            let current_file = platform::open_file_read(&self.directory, Path::new(&name))?;
            platform::require_private_file(&current_file)?;
            if platform::identity(&file)? != platform::identity(&current_file)? {
                return Err(io::Error::other("temporary recording settings changed"));
            }
            drop(current_file);
            platform::publish_file(
                &self.directory,
                &file,
                Path::new(&name),
                Path::new(SETTINGS_NAME),
                current.is_some(),
            )?;
            published = true;
            let (snapshot, actual) = self.read_current()?;
            if actual != *settings
                || snapshot
                    .as_ref()
                    .map(|value| (value.identity, value.digest))
                    != Some((platform::identity(&file)?, Sha256::digest(&bytes).into()))
            {
                return Err(io::Error::other(
                    "saved recording settings could not be verified",
                ));
            }
            self.snapshot = snapshot;
            platform::sync_directory(&self.directory)?;
            self.check_directory()
        })();
        if !published {
            if let Err(cleanup) = platform::remove_file(&self.directory, &file, Path::new(&name)) {
                return Err(io::Error::other(format!(
                    "{}; temporary recording settings could not be removed: {cleanup}",
                    result.expect_err("an unpublished save failed")
                )));
            }
        }
        result
    }

    fn check_directory(&self) -> io::Result<()> {
        platform::check_directory(&self.directory_path, &self.directory)
    }

    fn lock(&self) -> io::Result<File> {
        let name = Path::new(LOCK_NAME);
        let file = match platform::create_file(&self.directory, name) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                platform::open_file_write(&self.directory, name)?
            }
            Err(error) => return Err(error),
        };
        platform::require_private_file(&file)?;
        fs2::FileExt::try_lock_exclusive(&file)?;
        let current = platform::open_file_read(&self.directory, name)?;
        platform::require_private_file(&current)?;
        if platform::identity(&file)? != platform::identity(&current)? {
            return Err(io::Error::other("recording settings lock changed"));
        }
        Ok(file)
    }

    fn read_current(&self) -> io::Result<(Option<Snapshot>, RecordingSettings)> {
        let file = match platform::open_file_read(&self.directory, Path::new(SETTINGS_NAME)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok((None, RecordingSettings::default()));
            }
            Err(error) => return Err(error),
        };
        platform::require_private_file(&file)?;
        let identity = platform::identity(&file)?;
        if file.metadata()?.len() > MAX_SETTINGS_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "recording settings are too large",
            ));
        }
        let mut bytes = Vec::new();
        file.take(MAX_SETTINGS_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_SETTINGS_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "recording settings are too large",
            ));
        }
        let stored: SettingsFile = serde_json::from_slice(&bytes).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("recording settings are invalid: {error}"),
            )
        })?;
        if stored.version != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "recording settings use an unsupported version",
            ));
        }
        stored.settings.validate().map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("recording settings are invalid: {error}"),
            )
        })?;
        Ok((
            Some(Snapshot {
                identity,
                digest: Sha256::digest(&bytes).into(),
            }),
            stored.settings,
        ))
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    pub(super) fn open_directory(path: &Path) -> io::Result<File> {
        let directory = File::open(path)?;
        check_directory(path, &directory)?;
        Ok(directory)
    }

    pub(super) fn check_directory(path: &Path, directory: &File) -> io::Result<()> {
        if !festerm_unix_security::is_current_user_only(directory, true)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "recording settings directory is not private",
            ));
        }
        let current = fs::symlink_metadata(path)?;
        let pinned = directory.metadata()?;
        if !current.is_dir() || current.dev() != pinned.dev() || current.ino() != pinned.ino() {
            return Err(io::Error::other("recording settings directory changed"));
        }
        Ok(())
    }

    pub(super) fn create_file(directory: &File, name: &Path) -> io::Result<File> {
        festerm_unix_security::open_private_file(directory, name, true)
    }

    pub(super) fn open_file_write(directory: &File, name: &Path) -> io::Result<File> {
        festerm_unix_security::open_private_file(directory, name, false)
    }

    pub(super) fn open_file_for_removal(directory: &File, name: &Path) -> io::Result<File> {
        open_file_read(directory, name)
    }

    pub(super) fn open_file_read(directory: &File, name: &Path) -> io::Result<File> {
        festerm_unix_security::open_file_nofollow(directory, name)
    }

    pub(super) fn require_private_file(file: &File) -> io::Result<()> {
        if festerm_unix_security::is_current_user_only(file, false)? {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "recording settings file is not private",
            ))
        }
    }

    pub(super) fn identity(file: &File) -> io::Result<(u64, u64)> {
        let metadata = file.metadata()?;
        Ok((metadata.dev(), metadata.ino()))
    }

    pub(super) fn publish_file(
        directory: &File,
        _file: &File,
        source: &Path,
        target: &Path,
        replace: bool,
    ) -> io::Result<()> {
        if replace {
            festerm_unix_security::replace_private_file(directory, source, target)
        } else {
            festerm_unix_security::rename_noreplace(directory, source, directory, target)
        }
    }

    pub(super) fn remove_file(directory: &File, file: &File, name: &Path) -> io::Result<()> {
        let current = open_file_read(directory, name)?;
        require_private_file(&current)?;
        if identity(file)? != identity(&current)? {
            return Err(io::Error::other("temporary recording settings changed"));
        }
        festerm_unix_security::remove_private_staging_file(directory, name)
    }

    pub(super) fn sync_directory(directory: &File) -> io::Result<()> {
        directory.sync_all()
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use festerm_windows_security as security;

    pub(super) fn open_directory(path: &Path) -> io::Result<File> {
        let directory = security::lock_directory_path_without_delete_sharing(path)?;
        check_directory(path, &directory)?;
        Ok(directory)
    }

    pub(super) fn check_directory(_path: &Path, directory: &File) -> io::Result<()> {
        if directory.metadata()?.is_dir() && security::is_current_user_only(directory)? {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "recording settings directory is not private",
            ))
        }
    }

    pub(super) fn create_file(directory: &File, name: &Path) -> io::Result<File> {
        security::create_current_user_only_file(directory, name)
    }

    pub(super) fn open_file_write(directory: &File, name: &Path) -> io::Result<File> {
        security::open_file_read_write_no_reparse(directory, name)
    }

    pub(super) fn open_file_for_removal(directory: &File, name: &Path) -> io::Result<File> {
        security::open_file_no_reparse_for_verification(directory, name)
    }

    pub(super) fn open_file_read(directory: &File, name: &Path) -> io::Result<File> {
        security::open_file_no_reparse(directory, name)
    }

    pub(super) fn require_private_file(file: &File) -> io::Result<()> {
        if file.metadata()?.is_file()
            && security::is_current_user_only(file)?
            && security::file_link_count(file)? == 1
        {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "recording settings file is not private",
            ))
        }
    }

    pub(super) fn identity(file: &File) -> io::Result<(u64, u64)> {
        let (volume, index) = security::file_identity(file)?;
        Ok((u64::from(volume), index))
    }

    pub(super) fn publish_file(
        directory: &File,
        file: &File,
        _source: &Path,
        target: &Path,
        replace: bool,
    ) -> io::Result<()> {
        if replace {
            security::replace_private_file(file, directory, target)
        } else {
            security::rename_file_noreplace(file, directory, target)
        }
    }

    pub(super) fn remove_file(_directory: &File, file: &File, _name: &Path) -> io::Result<()> {
        security::remove_private_staging_file(file)
    }

    pub(super) fn sync_directory(directory: &File) -> io::Result<()> {
        security::lock_directory_for_sync(directory)?.sync_all()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::ExportDestination;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn private_root() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        festerm_unix_security::make_private_directory(&File::open(root.path()).unwrap()).unwrap();
        root
    }

    #[test]
    fn saves_and_reloads_device_local_settings() {
        let root = private_root();
        let (mut store, defaults) = RecordingSettingsStore::open(root.path()).unwrap();
        assert_eq!(defaults, RecordingSettings::default());
        let updated = RecordingSettings {
            destination: Some(ExportDestination::DesktopPath(PathBuf::from("/tmp/videos"))),
            normalize_typing: true,
            delete_source_after_export: false,
            ..defaults
        };
        store.save(&updated).unwrap();
        let (_, reloaded) = RecordingSettingsStore::open(root.path()).unwrap();
        assert_eq!(reloaded, updated);
        assert_eq!(
            fs::metadata(root.path().join(SETTINGS_NAME))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn refuses_concurrent_changes_and_invalid_settings() {
        let root = private_root();
        let (mut first, defaults) = RecordingSettingsStore::open(root.path()).unwrap();
        let (mut second, _) = RecordingSettingsStore::open(root.path()).unwrap();
        let updated = RecordingSettings {
            normalize_typing: true,
            ..defaults.clone()
        };
        first.save(&updated).unwrap();
        assert!(second.save(&defaults).is_err());
        assert_eq!(second.reload().unwrap(), updated);
        let mut invalid = updated.clone();
        invalid.typing_characters_per_second = 0;
        assert!(second.save(&invalid).is_err());
        assert_eq!(second.reload().unwrap(), updated);
    }

    #[test]
    fn refuses_unprivate_symlinked_or_corrupt_state() {
        let root = private_root();
        let link = root.path().with_extension("link");
        symlink(root.path(), &link).unwrap();
        assert!(RecordingSettingsStore::open(&link).is_err());
        fs::remove_file(link).unwrap();

        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(RecordingSettingsStore::open(root.path()).is_err());
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let (mut store, settings) = RecordingSettingsStore::open(root.path()).unwrap();
        store.save(&settings).unwrap();
        fs::write(root.path().join(SETTINGS_NAME), b"{bad").unwrap();
        assert!(store.reload().is_err());
        assert!(store.save(&settings).is_err());
    }

    #[test]
    fn refuses_a_busy_store_and_a_hard_linked_settings_file() {
        let root = private_root();
        let (mut first, defaults) = RecordingSettingsStore::open(root.path()).unwrap();
        first.save(&defaults).unwrap();
        let (mut second, _) = RecordingSettingsStore::open(root.path()).unwrap();
        let lock = first.lock().unwrap();
        assert!(second.reload().is_err());
        drop(lock);
        fs::hard_link(
            root.path().join(SETTINGS_NAME),
            root.path().join("another-name"),
        )
        .unwrap();
        assert!(second.reload().is_err());
    }

    #[test]
    fn refuses_replaced_directory_without_writing_to_it() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("recordings");
        let old = parent.path().join("old");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let (mut store, settings) = RecordingSettingsStore::open(&root).unwrap();
        fs::rename(&root, &old).unwrap();
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(store.save(&settings).is_err());
        assert!(!root.join(SETTINGS_NAME).exists());
        assert!(!old.join(SETTINGS_NAME).exists());
    }
}
