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
const LOCK_ATTEMPTS: u32 = 20;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    Starting,
    Ready,
    BreakawayGranted,
    BreakawayDenied,
    StartupChildExited {
        code: Option<i32>,
    },
    ShellExited {
        code: u32,
    },
    StopRequested,
    Stopping,
    Panic,
    Failure {
        stage: FailureStage,
        os_code: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recovery: Option<RecoveryFailureCategory>,
    },
    Shutdown {
        success: bool,
    },
}

/// Finite recovery-encoder evidence, never terminal content or an error string.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryFailureCategory {
    Validation,
    Budget,
    Allocation,
    Serialization,
    Length,
    #[serde(other)]
    Unclassified,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FailureStage {
    Runtime,
    Registry,
    Staging,
    IndependentSpawn,
    SpawnEvidence,
    HostLease,
    Registration,
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

fn storage(root: &Path, name: &str) -> io::Result<(File, PathBuf, private_storage::UnlockOnDrop)> {
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
    let lock = private_storage::UnlockOnDrop(lock);
    // Diagnostics must not stall PTY handling or shutdown behind another
    // process, but a concurrent reader/writer only holds the lock momentarily.
    let mut attempts = 0;
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => break,
            Err(error) if crate::lock_is_contended(&error) && attempts < LOCK_ATTEMPTS => {
                attempts += 1;
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(error) => return Err(error),
        }
    }
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
    record_actor(
        root,
        &identity.name,
        identity.pid,
        identity.created_at_unix_ms,
        event,
    )
}

/// Content-free diagnostics only: an actor is not a native-session identity or
/// evidence that a shell exists. Readers still require an exact daemon identity.
pub fn record_actor(
    root: &Path,
    name: &str,
    pid: u32,
    generation: u128,
    event: Event,
) -> io::Result<()> {
    let (directory, name, _lock) = storage(root, name)?;
    let mut entries = read(&directory, &name)?;
    if entries.len() == LIMIT {
        entries.remove(0);
    }
    entries.push(Entry {
        timestamp_ms: now_ms(),
        pid,
        generation,
        event,
    });
    let bytes = serde_json::to_vec(&entries)?;
    if bytes.len() > BYTE_LIMIT {
        return Err(io::Error::other("oversized lifecycle journal"));
    }
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
    fn recovery_failure_journal_is_compatible_content_free_and_stop_safe() {
        // This is the previous Failure payload: unknown nested fields are
        // ignored by old readers; old writers can drop the new category.
        #[derive(Deserialize, Serialize)]
        #[serde(rename_all = "snake_case")]
        enum LegacyEvent {
            Failure {
                stage: FailureStage,
                os_code: Option<i32>,
            },
        }
        let event = Event::Failure {
            stage: FailureStage::Daemon,
            os_code: None,
            recovery: Some(RecoveryFailureCategory::Validation),
        };
        let bytes = serde_json::to_vec(&event).unwrap();
        let legacy: LegacyEvent = serde_json::from_slice(&bytes).unwrap();
        let rewritten = serde_json::to_vec(&legacy).unwrap();
        let reread: Event = serde_json::from_slice(&rewritten).unwrap();
        assert_eq!(
            reread,
            Event::Failure {
                stage: FailureStage::Daemon,
                os_code: None,
                recovery: None,
            }
        );
        let future: Event = serde_json::from_str(
            r#"{"failure":{"stage":"daemon","os_code":null,"recovery":"future_category"}}"#,
        )
        .unwrap();
        assert_eq!(
            future,
            Event::Failure {
                stage: FailureStage::Daemon,
                os_code: None,
                recovery: Some(RecoveryFailureCategory::Unclassified),
            }
        );

        let root = private_storage::test_directory();
        let identity = NativeSessionIdentity {
            name: "recovery".into(),
            pid: 8,
            created_at_unix_ms: 9,
            endpoint: "unused".into(),
        };
        record(&root, &identity, Event::StopRequested).unwrap();
        record(&root, &identity, event.clone()).unwrap();
        record(&root, &identity, future).unwrap();
        let history = history(&root, &identity).unwrap();
        assert_eq!(history[1].event, event);
        assert!(!unexpected_exit(&history));
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("lifecycle/recovery.json")).unwrap())
                .unwrap();
        assert_eq!(value[1]["event"]["failure"].as_object().unwrap().len(), 3);
        assert_eq!(value[1]["event"]["failure"]["recovery"], "validation");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn starter_diagnostics_are_numeric_bounded_and_never_daemon_identity() {
        let root = private_storage::test_directory();
        let identity = NativeSessionIdentity {
            name: "test".into(),
            pid: 8,
            created_at_unix_ms: 9,
            endpoint: "unused".into(),
        };
        for _ in 0..70 {
            record_actor(
                &root,
                "test",
                7,
                9,
                Event::Failure {
                    stage: FailureStage::IndependentSpawn,
                    os_code: Some(5),
                    recovery: None,
                },
            )
            .unwrap();
        }
        record_actor(
            &root,
            "test",
            7,
            9,
            Event::StartupChildExited { code: Some(2) },
        )
        .unwrap();
        assert!(history(&root, &identity).unwrap().is_empty());
        assert!(!unexpected_exit(&history(&root, &identity).unwrap()));
        let bytes = std::fs::read(root.join("lifecycle/test.json")).unwrap();
        assert!(bytes.len() <= BYTE_LIMIT);
        let entries: Vec<Entry> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(entries.len(), LIMIT);
        assert!(entries
            .iter()
            .all(|entry| entry.pid == 7 && entry.generation == 9));
        assert_eq!(
            entries[0].event,
            Event::Failure {
                stage: FailureStage::IndependentSpawn,
                os_code: Some(5),
                recovery: None,
            }
        );
        assert_eq!(
            entries.last().unwrap().event,
            Event::StartupChildExited { code: Some(2) }
        );
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value[0].as_object().unwrap().len(), 4);
        assert_eq!(value[0]["event"]["failure"].as_object().unwrap().len(), 2);
        // Previous schema records continue to deserialize without migration.
        let old: Event = serde_json::from_str("\"ready\"").unwrap();
        assert_eq!(old, Event::Ready);
        std::fs::remove_dir_all(root).unwrap();
    }

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

    #[test]
    fn lifecycle_lock_waits_out_brief_contention_but_not_a_stuck_holder() {
        let root = private_storage::test_directory();
        let identity = NativeSessionIdentity {
            name: "test".into(),
            pid: 7,
            created_at_unix_ms: 9,
            endpoint: "unused".into(),
        };
        record(&root, &identity, Event::Ready).unwrap();
        let (_, _, held) = storage(&root, &identity.name).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            drop(held);
        });
        record(&root, &identity, Event::Starting).unwrap();
        release.join().unwrap();
        assert_eq!(history(&root, &identity).unwrap().len(), 2);

        let (_, _, _stuck) = storage(&root, &identity.name).unwrap();
        assert!(crate::lock_is_contended(
            &history(&root, &identity).unwrap_err()
        ));
        std::fs::remove_dir_all(root).unwrap();
    }
}
