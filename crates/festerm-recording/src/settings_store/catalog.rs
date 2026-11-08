use std::{
    fs,
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{platform, RecordingLease, Snapshot, MAX_SETTINGS_BYTES};
use crate::{LocalStart, RecordingId, RecordingSettings};

const VERSION: u8 = 1;
const PREFIX: &str = "recording-meta-";
const SUFFIX: &str = ".json";
const MAX_MANIFESTS: usize = 4096;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    version: u8,
    id: String,
    start: LocalStart,
    settings: RecordingSettings,
}

/// An immutable, device-local snapshot saved before admitting capture.
///
/// The host establishes private, backup-excluded storage before creating or
/// loading manifests. A manifest contains destination authorization material
/// and must be treated as sensitive.
pub struct RecordingManifest {
    directory_path: PathBuf,
    directory: File,
    name: String,
    snapshot: Snapshot,
    id: RecordingId,
    start: LocalStart,
    settings: RecordingSettings,
}

impl RecordingManifest {
    /// Saves a new recording's destination, names, pacing and retention
    /// policy without overwriting an existing manifest.
    pub fn create(
        directory_path: &Path,
        id: RecordingId,
        start: LocalStart,
        settings: &RecordingSettings,
    ) -> io::Result<Self> {
        validate(start, settings)?;
        let directory = platform::open_directory(directory_path)?;
        let name = format!("{PREFIX}{id}{SUFFIX}");
        let value = ManifestFile {
            version: VERSION,
            id: id.to_string(),
            start,
            settings: settings.clone(),
        };
        let bytes = serde_json::to_vec(&value).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_SETTINGS_BYTES {
            return Err(invalid("recording manifest is too large"));
        }
        let temporary = format!(".recording-meta-{}.tmp", Uuid::new_v4().simple());
        let mut file = platform::create_file(&directory, Path::new(&temporary))?;
        let mut published = false;
        let result = (|| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            platform::check_directory(directory_path, &directory)?;
            let current = platform::open_file_read(&directory, Path::new(&temporary))?;
            platform::require_private_file(&current)?;
            let identity = platform::identity(&file)?;
            if identity != platform::identity(&current)? {
                return Err(invalid("temporary recording manifest changed"));
            }
            drop(current);
            platform::publish_file(
                &directory,
                &file,
                Path::new(&temporary),
                Path::new(&name),
                false,
            )?;
            published = true;
            let manifest = read(directory_path, directory.try_clone()?, &name)?;
            let expected: [u8; 32] = Sha256::digest(&bytes).into();
            if manifest.snapshot.identity != identity || manifest.snapshot.digest != expected {
                return Err(invalid("saved recording manifest could not be verified"));
            }
            platform::sync_directory(&directory)?;
            platform::check_directory(directory_path, &directory)?;
            Ok(manifest)
        })();
        if !published {
            if let Err(cleanup) = platform::remove_file(&directory, &file, Path::new(&temporary)) {
                return Err(io::Error::other(format!(
                    "{}; temporary recording manifest could not be removed: {cleanup}",
                    result.err().expect("an unpublished manifest failed")
                )));
            }
        }
        result
    }

    /// Loads every manifest. Malformed, unsafe, or changed entries are
    /// reported rather than silently hiding recordings from recovery.
    pub fn recover(directory_path: &Path) -> io::Result<Vec<Self>> {
        let directory = platform::open_directory(directory_path)?;
        let mut names = Vec::new();
        for entry in fs::read_dir(directory_path)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.starts_with(PREFIX) && name.ends_with(SUFFIX) {
                names.push(name.to_owned());
                if names.len() > MAX_MANIFESTS {
                    return Err(invalid("too many recording manifests"));
                }
            }
        }
        platform::check_directory(directory_path, &directory)?;
        names.sort();
        names
            .into_iter()
            .map(|name| read(directory_path, directory.try_clone()?, &name))
            .collect()
    }

    pub const fn id(&self) -> RecordingId {
        self.id
    }

    pub const fn start(&self) -> LocalStart {
        self.start
    }

    pub fn settings(&self) -> &RecordingSettings {
        &self.settings
    }

    pub fn verify(&self, lease: &RecordingLease) -> io::Result<()> {
        lease.verify_for(&self.directory_path)?;
        let current = read(
            &self.directory_path,
            self.directory.try_clone()?,
            &self.name,
        )?;
        if current.snapshot != self.snapshot {
            return Err(invalid("recording manifest changed and was retained"));
        }
        lease.verify_for(&self.directory_path)
    }

    /// Removes only the unchanged manifest bound to this handle. The host
    /// must first complete or explicitly discard the associated recording.
    pub fn remove(self) -> io::Result<()> {
        platform::check_directory(&self.directory_path, &self.directory)?;
        let current = read(
            &self.directory_path,
            self.directory.try_clone()?,
            &self.name,
        )?;
        if current.snapshot != self.snapshot {
            return Err(invalid("recording manifest changed and was retained"));
        }
        let file = platform::open_file_for_removal(&self.directory, Path::new(&self.name))?;
        platform::require_private_file(&file)?;
        if platform::identity(&file)? != self.snapshot.identity {
            return Err(invalid("recording manifest changed and was retained"));
        }
        platform::remove_file(&self.directory, &file, Path::new(&self.name))?;
        platform::sync_directory(&self.directory)?;
        platform::check_directory(&self.directory_path, &self.directory)
    }
}

fn read(directory_path: &Path, directory: File, name: &str) -> io::Result<RecordingManifest> {
    let id = name
        .strip_prefix(PREFIX)
        .and_then(|value| value.strip_suffix(SUFFIX))
        .and_then(|value| RecordingId::parse(value).ok())
        .ok_or_else(|| invalid("invalid recording manifest name"))?;
    if name != format!("{PREFIX}{id}{SUFFIX}") {
        return Err(invalid("invalid recording manifest name"));
    }
    let file = platform::open_file_read(&directory, Path::new(name))?;
    platform::require_private_file(&file)?;
    let identity = platform::identity(&file)?;
    if file.metadata()?.len() > MAX_SETTINGS_BYTES {
        return Err(invalid("recording manifest is too large"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_SETTINGS_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        return Err(invalid("recording manifest is too large"));
    }
    let stored: ManifestFile =
        serde_json::from_slice(&bytes).map_err(|_| invalid("recording manifest is invalid"))?;
    if stored.version != VERSION || stored.id != id.to_string() {
        return Err(invalid("recording manifest identity or version is invalid"));
    }
    validate(stored.start, &stored.settings)?;
    platform::check_directory(directory_path, &directory)?;
    Ok(RecordingManifest {
        directory_path: directory_path.to_owned(),
        directory,
        name: name.to_owned(),
        snapshot: Snapshot {
            identity,
            digest: Sha256::digest(&bytes).into(),
        },
        id,
        start: stored.start,
        settings: stored.settings,
    })
}

fn validate(start: LocalStart, settings: &RecordingSettings) -> io::Result<()> {
    start
        .validate()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    settings
        .validate()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if settings.destination.is_none() {
        return Err(invalid("a recording export folder must be selected"));
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::ExportDestination;

    fn settings() -> RecordingSettings {
        RecordingSettings {
            destination: Some(ExportDestination::AndroidTree(
                "content://provider/tree/example".to_owned(),
            )),
            ..RecordingSettings::default()
        }
    }

    #[test]
    fn persists_the_exact_snapshot_before_capture_without_replacing_it() {
        let root = tempfile::tempdir().unwrap();
        festerm_unix_security::make_private_directory(&File::open(root.path()).unwrap()).unwrap();
        let id = RecordingId::new();
        let start = LocalStart::new(2026, 2, 28, 9, 6, 5).unwrap();
        let initial = settings();
        let manifest = RecordingManifest::create(root.path(), id, start, &initial).unwrap();
        let mut changed = initial.clone();
        changed.normalize_typing = true;
        assert!(RecordingManifest::create(root.path(), id, start, &changed).is_err());
        let recovered = RecordingManifest::recover(root.path()).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].id(), id);
        assert_eq!(recovered[0].start(), start);
        assert_eq!(recovered[0].settings(), &initial);
        manifest.remove().unwrap();
        assert!(RecordingManifest::recover(root.path()).unwrap().is_empty());
    }

    #[test]
    fn refuses_changed_or_corrupt_manifests_and_missing_destinations() {
        let root = tempfile::tempdir().unwrap();
        festerm_unix_security::make_private_directory(&File::open(root.path()).unwrap()).unwrap();
        let id = RecordingId::new();
        let start = LocalStart::new(2026, 2, 28, 9, 6, 5).unwrap();
        assert!(
            RecordingManifest::create(root.path(), id, start, &RecordingSettings::default())
                .is_err()
        );
        let manifest = RecordingManifest::create(root.path(), id, start, &settings()).unwrap();
        let path = root.path().join(format!("{PREFIX}{id}{SUFFIX}"));
        fs::write(&path, b"{}").unwrap();
        assert!(RecordingManifest::recover(root.path()).is_err());
        assert!(manifest.remove().is_err());
        assert!(path.exists());
    }
}
