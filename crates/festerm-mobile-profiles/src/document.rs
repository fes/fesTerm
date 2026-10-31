//! Bounded, atomic, versioned JSON documents in the app sandbox.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;
use uuid::Uuid;

use crate::error::StorageError;

pub(crate) type Migration = fn(&mut Value) -> Result<(), StorageError>;

pub(crate) enum Loaded {
    Missing,
    /// Unparseable or structurally wrong; the file was moved aside.
    Corrupt {
        quarantined: Option<PathBuf>,
    },
    Document {
        value: Value,
        migrated_from: Option<u32>,
    },
}

pub(crate) fn read_bounded(path: &Path, max: usize) -> Result<Option<Vec<u8>>, StorageError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(StorageError::Io),
    };
    if !metadata.is_file() {
        return Err(StorageError::Io);
    }
    if metadata.len() > max as u64 {
        return Err(StorageError::TooLarge);
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(max as u64 + 1).read_to_end(&mut bytes))
        .map_err(|_| StorageError::Io)?;
    if bytes.len() > max {
        return Err(StorageError::TooLarge);
    }
    Ok(Some(bytes))
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8], max: usize) -> Result<(), StorageError> {
    if bytes.len() > max {
        return Err(StorageError::TooLarge);
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(StorageError::Io)?;
    let name = path.file_name().ok_or(StorageError::Io)?.to_string_lossy();
    let temporary = parent.join(format!(".{name}.{}.tmp", Uuid::new_v4().simple()));
    let result = write_and_replace(&temporary, path, bytes);
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    } else {
        sync_directory(parent);
    }
    result.map_err(|_| StorageError::Io)
}

fn write_and_replace(temporary: &Path, target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, target)
}

#[cfg(unix)]
fn sync_directory(directory: &Path) {
    if let Ok(directory) = File::open(directory) {
        let _ = directory.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_directory(_: &Path) {}

fn quarantine_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".corrupt");
    PathBuf::from(name)
}

/// Keeps the most recent unreadable copy for support; it replaces any earlier
/// one so quarantines cannot accumulate.
pub(crate) fn quarantine_copy(path: &Path) -> Option<PathBuf> {
    let target = quarantine_path(path);
    fs::copy(path, &target).ok().map(|_| target)
}

/// Loads `path`, applying `migrations` (entry `n` upgrades version `n + 1`).
/// Versions newer than `current` are refused untouched.
pub(crate) fn load_document(
    path: &Path,
    max: usize,
    current: u32,
    migrations: &[Migration],
) -> Result<Loaded, StorageError> {
    let Some(bytes) = read_bounded(path, max)? else {
        return Ok(Loaded::Missing);
    };
    let corrupt = || {
        let quarantined = fs::rename(path, quarantine_path(path))
            .ok()
            .map(|()| quarantine_path(path));
        Ok(Loaded::Corrupt { quarantined })
    };
    let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
        return corrupt();
    };
    let Some(version) = value
        .get("version")
        .and_then(Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .filter(|version| *version >= 1)
    else {
        return corrupt();
    };
    if version > current {
        return Err(StorageError::UnsupportedVersion { found: version });
    }
    let mut migrated_from = None;
    for from in version..current {
        let migration = migrations.get(from as usize - 1).ok_or(StorageError::Io)?;
        migration(&mut value)?;
        value["version"] = Value::from(from + 1);
        migrated_from = Some(version);
    }
    Ok(Loaded::Document {
        value,
        migrated_from,
    })
}
