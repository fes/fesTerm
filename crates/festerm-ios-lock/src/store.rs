use crate::policy::LockSettings;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const MAX_FILE_BYTES: u64 = 4096;
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Document {
    version: u32,
    #[serde(flatten)]
    settings: LockSettings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadStatus {
    /// No settings file yet: defaults (lock off).
    Missing,
    Loaded,
    /// Unreadable, malformed, oversized or from a newer version: fail closed.
    Untrusted,
}

/// Persisted lock settings at an injected path (the app's private sandbox).
#[derive(Clone, Debug)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> (LockSettings, LoadStatus) {
        match self.read() {
            Ok(None) => (LockSettings::default(), LoadStatus::Missing),
            Ok(Some(settings)) => (settings, LoadStatus::Loaded),
            Err(_) => (LockSettings::fail_closed(), LoadStatus::Untrusted),
        }
    }

    fn read(&self) -> io::Result<Option<LockSettings>> {
        let file = match fs::File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let document: Document = serde_json::from_slice(&bytes)
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        if document.version != VERSION {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(Some(document.settings))
    }

    /// Atomic replace (temp file in the same directory, then rename).
    pub fn save(&self, settings: &LockSettings) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(&Document {
            version: VERSION,
            settings: *settings,
        })
        .map_err(io::Error::other)?;
        let parent = self.path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let result = (|| {
            let mut options = fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            let mut file = options.open(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&tmp, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}
