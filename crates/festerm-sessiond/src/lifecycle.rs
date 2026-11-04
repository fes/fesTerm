//! Bounded, content-free lifecycle evidence, retained after a daemon exits.
use crate::{private_storage, NativeSessionIdentity};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const LIMIT: usize = 64;
const BYTE_LIMIT: usize = 32 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    Starting,
    Ready,
    BreakawayGranted,
    BreakawayDenied,
    ShellExited {
        code: u32,
    },
    StopRequested,
    Stopping,
    Panic,
    Failure {
        stage: FailureStage,
        os_code: Option<i32>,
    },
    Shutdown {
        success: bool,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FailureStage {
    Daemon,
    ShellSpawn,
    ShellWait,
    PtyRead,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub timestamp_ms: u128,
    pub pid: u32,
    pub generation: u128,
    pub event: Event,
}

pub fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_millis())
}

fn storage(root: &Path, name: &str) -> io::Result<(File, PathBuf, File)> {
    festerm_ssh::PersistentSessionName::new(name)
        .map_err(|_| io::Error::other("invalid lifecycle identity"))?;
    let directory = private_storage::private_directory(&root.join("lifecycle"), true)?;
    let lock_name = PathBuf::from(format!("{name}.lock"));
    let lock = match private_storage::open_private_file(&directory, &lock_name, true) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            private_storage::open_private_file(&directory, &lock_name, false)?
        }
        Err(error) => return Err(error),
    };
    // Diagnostics must not stall PTY handling or shutdown behind another process.
    lock.try_lock_exclusive()?;
    Ok((directory, PathBuf::from(format!("{name}.json")), lock))
}

fn read(directory: &File, name: &Path) -> io::Result<Vec<Entry>> {
    match private_storage::read_private(directory, name, BYTE_LIMIT) {
        Ok(bytes) => {
            let entries: Vec<Entry> = serde_json::from_slice(&bytes)
                .map_err(|_| io::Error::other("invalid lifecycle journal"))?;
            if entries.len() > LIMIT {
                return Err(io::Error::other("oversized lifecycle journal"));
            }
            Ok(entries)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

pub fn record(root: &Path, identity: &NativeSessionIdentity, event: Event) -> io::Result<()> {
    let (directory, name, _lock) = storage(root, &identity.name)?;
    let mut entries = read(&directory, &name)?;
    if entries.len() == LIMIT {
        entries.remove(0);
    }
    entries.push(Entry {
        timestamp_ms: now_ms(),
        pid: identity.pid,
        generation: identity.created_at_unix_ms,
        event,
    });
    let bytes = serde_json::to_vec(&entries)?;
    private_storage::replace_private(&directory, &name, &bytes)
}

pub fn history(root: &Path, identity: &NativeSessionIdentity) -> io::Result<Vec<Entry>> {
    let (directory, name, _lock) = storage(root, &identity.name)?;
    Ok(read(&directory, &name)?
        .into_iter()
        .filter(|entry| {
            entry.pid == identity.pid && entry.generation == identity.created_at_unix_ms
        })
        .collect())
}

/// Missing/corrupt diagnostics never authorize replacement. Any deliberate stop
/// wins, even if a later cleanup failure or panic follows it.
pub fn unexpected_exit(entries: &[Entry]) -> bool {
    !entries.is_empty()
        && !entries.iter().any(|entry| {
            matches!(
                entry.event,
                Event::ShellExited { .. }
                    | Event::StopRequested
                    | Event::Stopping
                    | Event::Shutdown { success: true }
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_journal_is_bounded_generation_scoped_and_preserves_stop_intent() {
        let root = private_storage::test_directory();
        let mut identity = NativeSessionIdentity {
            name: "test".into(),
            pid: 7,
            created_at_unix_ms: 9,
            endpoint: "unused".into(),
        };
        assert!(!unexpected_exit(&history(&root, &identity).unwrap()));
        record(&root, &identity, Event::Starting).unwrap();
        assert!(unexpected_exit(&history(&root, &identity).unwrap()));
        record(&root, &identity, Event::StopRequested).unwrap();
        record(&root, &identity, Event::Panic).unwrap();
        assert!(!unexpected_exit(&history(&root, &identity).unwrap()));
        identity.created_at_unix_ms = 10;
        assert!(history(&root, &identity).unwrap().is_empty());
        for _ in 0..70 {
            record(&root, &identity, Event::Ready).unwrap();
        }
        assert_eq!(history(&root, &identity).unwrap().len(), LIMIT);
        for event in [
            Event::ShellExited { code: 0 },
            Event::ShellExited { code: 1 },
            Event::Shutdown { success: true },
        ] {
            record(&root, &identity, event).unwrap();
            assert!(!unexpected_exit(&history(&root, &identity).unwrap()));
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
