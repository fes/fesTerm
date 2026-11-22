//! Finite local parent-boundary evidence; never a daemon identity or authority.
use crate::lifecycle::{self, Entry, StarterIdentity};
use std::{fmt, io, path::Path};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupPhase {
    Runtime,
    Registry,
    Helper,
    Spawn,
    Wait,
    ChildExit,
    Attach,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum History {
    AttributionUnavailable,
    JournalUnavailable,
    Available(Vec<Entry>),
}

/// A bounded, content-free failure snapshot. No path, command, stderr, endpoint
/// string, or error message crosses this seam. A starter PID does not identify
/// the daemon, its shell, or a Windows breakaway successor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartupDiagnostics {
    phase: StartupPhase,
    starter_pid: Option<u32>,
    generation: Option<u128>,
    os_code: Option<i32>,
    child_exit: Option<i32>,
    history: History,
}

impl StartupDiagnostics {
    pub(crate) fn boundary(phase: StartupPhase, error: Option<&io::Error>) -> Self {
        Self {
            phase,
            starter_pid: None,
            generation: None,
            os_code: error.and_then(io::Error::raw_os_error),
            child_exit: None,
            history: History::AttributionUnavailable,
        }
    }

    pub(crate) fn for_attempt(
        mut self,
        root: &Path,
        name: &str,
        pid: u32,
        trusted_generation: Option<u128>,
        child_exit: Option<i32>,
    ) -> Self {
        self.starter_pid = Some(pid);
        self.generation = trusted_generation;
        self.child_exit = child_exit;
        if let Some(generation) = trusted_generation {
            self.history = match lifecycle::starter_history(
                root,
                name,
                &StarterIdentity { pid, generation },
            ) {
                Ok(entries) if !entries.is_empty() => History::Available(entries),
                _ => History::JournalUnavailable,
            };
        }
        self
    }

    pub fn phase(&self) -> StartupPhase {
        self.phase
    }
    pub fn starter_pid(&self) -> Option<u32> {
        self.starter_pid
    }
    pub fn generation(&self) -> Option<u128> {
        self.generation
    }
    pub fn os_code(&self) -> Option<i32> {
        self.os_code
    }
    pub fn child_exit(&self) -> Option<i32> {
        self.child_exit
    }
}

impl fmt::Display for StartupDiagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Local startup phase={:?}; starter_pid={:?}; generation={:?}; os={:?}; child_exit={:?}; daemon/endpoint=unavailable", self.phase, self.starter_pid, self.generation, self.os_code, self.child_exit)?;
        match &self.history {
            History::AttributionUnavailable => write!(
                f,
                "; lifecycle=unavailable (starter attribution incomplete)"
            ),
            History::JournalUnavailable => {
                write!(f, "; lifecycle=unavailable (journal not observed)")
            }
            History::Available(entries) => {
                for entry in entries {
                    write!(
                        f,
                        "\nStarter event {:?}; time_ms={}",
                        entry.event, entry.timestamp_ms
                    )?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{private_storage, PersistentSessionError};
    use fs2::FileExt;

    #[test]
    fn startup_snapshot_is_actor_scoped_bounded_and_not_a_daemon_history() {
        let root = private_storage::test_directory();
        for (pid, generation) in [(7, 9), (8, 9), (7, 10)] {
            lifecycle::record_actor(&root, "owned", pid, generation, lifecycle::Event::Starting)
                .unwrap();
        }
        let snapshot = StartupDiagnostics::boundary(StartupPhase::ChildExit, None).for_attempt(
            &root,
            "owned",
            7,
            Some(9),
            Some(23),
        );
        let History::Available(entries) = &snapshot.history else {
            panic!("owned starter history");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!((entries[0].pid, entries[0].generation), (7, 9));
        assert_eq!(snapshot.child_exit(), Some(23));
        for _ in 0..70 {
            lifecycle::record_actor(&root, "owned", 7, 9, lifecycle::Event::Starting).unwrap();
        }
        let snapshot = StartupDiagnostics::boundary(StartupPhase::ChildExit, None).for_attempt(
            &root,
            "owned",
            7,
            Some(9),
            None,
        );
        assert!(matches!(&snapshot.history, History::Available(entries) if entries.len() == 64));
        assert!(snapshot.to_string().len() < 32 * 1024);
        assert!(!snapshot.to_string().contains("owned"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_history_observation_never_creates_and_refuses_untrusted_or_busy_state() {
        let root = private_storage::test_directory();
        let absent = root.join("absent");
        let identity = StarterIdentity {
            pid: 7,
            generation: 9,
        };
        assert!(lifecycle::starter_history(&absent, "owned", &identity).is_err());
        assert!(!absent.exists());
        let early = StartupDiagnostics::boundary(StartupPhase::Spawn, None)
            .for_attempt(&root, "owned", 7, None, None);
        assert_eq!(early.history, History::AttributionUnavailable);
        assert!(!root.join("lifecycle").exists());
        assert!(lifecycle::starter_history(&root, "../other", &identity).is_err());
        assert!(!root.join("lifecycle").exists());
        lifecycle::record_actor(&root, "owned", 7, 9, lifecycle::Event::Starting).unwrap();
        let directory = private_storage::private_directory(&root.join("lifecycle"), false).unwrap();
        let lock =
            private_storage::open_private_file(&directory, Path::new("owned.lock"), false).unwrap();
        lock.try_lock_exclusive().unwrap();
        let busy = StartupDiagnostics::boundary(StartupPhase::ChildExit, None).for_attempt(
            &root,
            "owned",
            7,
            Some(9),
            None,
        );
        assert_eq!(busy.history, History::JournalUnavailable);
        FileExt::unlock(&lock).unwrap();
        private_storage::replace_private(&directory, Path::new("owned.json"), b"not-json secret")
            .unwrap();
        let corrupt = StartupDiagnostics::boundary(StartupPhase::ChildExit, None).for_attempt(
            &root,
            "owned",
            7,
            Some(9),
            None,
        );
        assert_eq!(corrupt.history, History::JournalUnavailable);
        assert!(!corrupt.to_string().contains("secret"));
        drop(lock);
        drop(directory);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_evidence_preserves_original_error_and_redacts_untrusted_message() {
        let error = PersistentSessionError::new("secret argv cwd stderr token")
            .at_recovery_boundary(lifecycle::RecoveryFailureCategory::Validation);
        let original = error.to_string();
        let error = error.with_startup_diagnostics(StartupDiagnostics::boundary(
            StartupPhase::Spawn,
            Some(&io::Error::from_raw_os_error(5)),
        ));
        assert_eq!(error.to_string(), original);
        assert_eq!(
            error.recovery_failure_category(),
            lifecycle::RecoveryFailureCategory::Validation
        );
        let snapshot = error.startup_diagnostics().unwrap();
        assert_eq!(snapshot.os_code(), Some(5));
        assert_eq!(snapshot.starter_pid(), None);
        assert_eq!(snapshot.generation(), None);
        for private in ["secret", "argv", "cwd", "stderr", "token"] {
            assert!(!snapshot.to_string().contains(private));
        }
    }

    #[test]
    fn actual_local_spawn_failure_retains_numeric_boundary_without_private_path() {
        let root = private_storage::test_directory();
        let missing = root.join("secret-helper-does-not-exist");
        let error = crate::run_local_start_command(
            &mut std::process::Command::new(&missing),
            &missing,
            &root,
            "private-name",
        )
        .unwrap_err();
        let snapshot = error.startup_diagnostics().unwrap();
        assert_eq!(snapshot.phase(), StartupPhase::Spawn);
        assert!(snapshot.os_code().is_some());
        assert_eq!(snapshot.starter_pid(), None);
        assert!(!snapshot.to_string().contains("secret-helper"));
        assert!(!snapshot.to_string().contains("private-name"));
        assert!(!root.join("lifecycle").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn actual_local_child_failure_retains_owned_pid_exit_and_original_stderr() {
        use std::process::{Command, Stdio};
        let root = private_storage::test_directory();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "printf '%s private-stderr' \"$$\" >&2; exit 23"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let error = crate::run_local_start_command(
            &mut command,
            Path::new("/bin/sh"),
            &root,
            "private-name",
        )
        .unwrap_err();
        let snapshot = error.startup_diagnostics().unwrap();
        assert_eq!(
            error.to_string(),
            format!("session daemon could not start 'private-name': {} private-stderr", snapshot.starter_pid().unwrap()),
            "the reported actor must be the exact child PID independently emitted by the owned fixture",
        );
        assert_eq!(snapshot.phase(), StartupPhase::ChildExit);
        assert!(snapshot.starter_pid().is_some_and(|pid| pid > 0));
        assert_eq!(snapshot.child_exit(), Some(23));
        assert_eq!(snapshot.generation(), None);
        assert!(!snapshot.to_string().contains("private-stderr"));
        assert!(!root.join("lifecycle").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
