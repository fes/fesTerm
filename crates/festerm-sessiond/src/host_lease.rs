//! Private generation-scoped daemon supervision. No network authority lives here.
use crate::private_storage as storage;
use festerm_host_protocol::{AttachmentId, GenerationId, Lifetime, OperationId, TemporaryLease};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const LIMIT: usize = 4096;
const POLL: Duration = Duration::from_millis(250);

fn invalid() -> io::Error {
    io::Error::other("host daemon lease unavailable or stale")
}

pub(crate) fn now_ms() -> u128 {
    crate::lifecycle::now_ms()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DaemonLease {
    pub version: u8,
    pub name: String,
    pub generation: GenerationId,
    pub revision: u64,
    pub lifetime: Lifetime,
    pub temporary: Option<TemporaryLease>,
    pub deadline_ms: Option<u128>,
    pub connected: bool,
    pub stopped: bool,
    pub remote_revoked: bool,
    pub pending: Option<PendingAttachment>,
    pub last_attachment: Option<OperationId>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingAttachment {
    pub ticket: OperationId,
    pub previous: TemporaryLease,
    pub attachment: AttachmentId,
    pub deadline_ms: u128,
}

impl DaemonLease {
    pub fn validate(&self) -> io::Result<()> {
        self.lifetime.validate().map_err(|_| invalid())?;
        if self.version != 1
            || self.revision == 0
            || festerm_ssh::PersistentSessionName::new(&self.name).is_err()
        {
            return Err(invalid());
        }
        match (self.lifetime, &self.temporary, self.deadline_ms) {
            (Lifetime::Persistent {}, None, None) if self.pending.is_none() => {}
            (Lifetime::Temporary { .. }, Some(_), Some(_)) => {}
            _ => return Err(invalid()),
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| Some(&pending.previous) != self.temporary.as_ref())
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn expired(&self) -> bool {
        self.stopped
            || self
                .deadline_ms
                .is_some_and(|deadline| deadline <= now_ms())
    }
}

pub(crate) struct LeaseGuard {
    directory: File,
    name: PathBuf,
    generation: GenerationId,
    _lock: File,
}

fn open_lock(directory: &File, name: &Path) -> io::Result<File> {
    match storage::open_private_file(directory, name, true) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            storage::open_private_file(directory, name, false)
        }
        Err(error) => Err(error),
    }
}

impl LeaseGuard {
    pub fn acquire(root: &Path, generation: GenerationId) -> io::Result<Self> {
        let directory = storage::private_directory(root, false)?;
        let stem = format!("host-daemon-{}", generation.get());
        let lock = open_lock(&directory, Path::new(&format!("{stem}.lock")))?;
        // Never queue a daemon or a control worker behind a stalled bridge.
        FileExt::try_lock_exclusive(&lock)?;
        Ok(Self {
            directory,
            name: PathBuf::from(format!("{stem}.json")),
            generation,
            _lock: lock,
        })
    }

    pub fn read(&self) -> io::Result<DaemonLease> {
        let bytes = storage::read_private(&self.directory, &self.name, LIMIT)?;
        let lease: DaemonLease = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        lease.validate()?;
        if lease.generation != self.generation {
            return Err(invalid());
        }
        Ok(lease)
    }

    pub fn create(&self, lease: &DaemonLease) -> io::Result<()> {
        lease.validate()?;
        let bytes = serde_json::to_vec(lease).map_err(|_| invalid())?;
        storage::write_new_private(&self.directory, &self.name, &bytes)
    }

    pub fn replace(&self, lease: &mut DaemonLease) -> io::Result<()> {
        lease.revision = lease.revision.checked_add(1).ok_or_else(invalid)?;
        lease.validate()?;
        let bytes = serde_json::to_vec(lease).map_err(|_| invalid())?;
        storage::replace_private(&self.directory, &self.name, &bytes)
    }
}

/// Owned by the actual daemon, not by the host bridge. Persisted wall-clock
/// deadlines recover after supervisor restart; monotonic caps prevent a clock
/// rollback or a contended lease lock extending a running lease indefinitely.
#[doc(hidden)]
pub struct DaemonWatch {
    root: PathBuf,
    name: String,
    generation: GenerationId,
    revision: u64,
    deadline: Option<Instant>,
    next_poll: Instant,
    stopped: bool,
}

impl DaemonWatch {
    pub fn open(root: &Path, name: &str, generation: u128) -> io::Result<Self> {
        let generation = GenerationId::new(generation).map_err(|_| invalid())?;
        let guard = LeaseGuard::acquire(root, generation)?;
        let lease = guard.read()?;
        if lease.name != name || lease.generation != generation {
            return Err(invalid());
        }

        let mut watch = Self {
            root: root.into(),
            name: name.into(),
            generation,
            revision: 0,
            deadline: None,
            next_poll: Instant::now(),
            stopped: false,
        };
        watch.adopt(&lease)?;
        Ok(watch)
    }

    fn adopt(&mut self, lease: &DaemonLease) -> io::Result<()> {
        if lease.name != self.name
            || lease.generation != self.generation
            || lease.revision < self.revision
        {
            return Err(invalid());
        }
        if lease.revision > self.revision {
            self.revision = lease.revision;
            self.deadline = lease.deadline_ms.map(|deadline| {
                let remaining = deadline.saturating_sub(now_ms()).min(60_000);
                Instant::now() + Duration::from_millis(remaining as u64)
            });
        }
        self.stopped |= lease.stopped;
        Ok(())
    }

    /// Validate a local admission ticket without changing lease ownership.
    pub fn allows_attachment(&self, ticket: Option<&OperationId>, remote: bool) -> bool {
        let Ok(guard) = LeaseGuard::acquire(&self.root, self.generation) else {
            return false;
        };
        let Ok(lease) = guard.read() else {
            return false;
        };
        if lease.expired() || (remote && lease.remote_revoked) {
            return false;
        }
        match (&lease.temporary, ticket) {
            (None, None) => true,
            (Some(_), Some(ticket)) => lease
                .pending
                .as_ref()
                .is_some_and(|pending| &pending.ticket == ticket && pending.deadline_ms > now_ms()),
            _ => false,
        }
    }

    /// Contention is not cancellation; commit still performs a strict check.
    pub fn attachment_cancelled(&self, ticket: Option<&OperationId>, remote: bool) -> bool {
        let guard = match LeaseGuard::acquire(&self.root, self.generation) {
            Ok(guard) => guard,
            Err(error) if crate::lock_is_contended(&error) => return false,
            Err(_) => return true,
        };
        let Ok(lease) = guard.read() else {
            return true;
        };
        if lease.expired() || (remote && lease.remote_revoked) {
            return true;
        }
        match (&lease.temporary, ticket) {
            (None, None) => false,
            (Some(_), Some(ticket)) => !lease
                .pending
                .as_ref()
                .is_some_and(|pending| &pending.ticket == ticket && pending.deadline_ms > now_ms()),
            _ => true,
        }
    }

    /// Called by the sole daemon writer loop after recovery adoption, directly
    /// before replacing the old writer. A failed transfer preserves that writer.
    pub fn commit_attachment(
        &mut self,
        ticket: Option<&OperationId>,
        remote: bool,
    ) -> io::Result<()> {
        let guard = LeaseGuard::acquire(&self.root, self.generation)?;
        let mut lease = guard.read()?;
        if lease.expired() || (remote && lease.remote_revoked) {
            return Err(invalid());
        }
        if lease.temporary.is_none() && ticket.is_none() {
            return Ok(());
        }
        let pending = lease.pending.take().ok_or_else(invalid)?;
        if Some(&pending.ticket) != ticket
            || pending.deadline_ms <= now_ms()
            || lease.temporary.as_ref() != Some(&pending.previous)
        {
            return Err(invalid());
        }
        let binding = lease.temporary.as_mut().ok_or_else(invalid)?;
        binding.epoch = binding.epoch.next().map_err(|_| invalid())?;
        binding.attachment = pending.attachment;
        lease.last_attachment = Some(pending.ticket);
        lease.connected = true;
        lease.deadline_ms = Some(now_ms() + 60_000);
        guard.replace(&mut lease)?;
        self.adopt(&lease)
    }

    /// Called even while terminal output is backpressured. Any malformed or
    /// missing protected state fails closed; ordinary legacy shells never opt in.
    pub fn should_stop(&mut self) -> bool {
        if self.stopped
            || self
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return true;
        }
        if Instant::now() < self.next_poll {
            return false;
        }
        self.next_poll = Instant::now() + POLL;
        let result = match LeaseGuard::acquire(&self.root, self.generation) {
            Ok(guard) => guard.read().and_then(|lease| self.adopt(&lease)),
            Err(error) if crate::lock_is_contended(&error) => Ok(()),
            Err(error) => Err(error),
        };
        self.stopped |= result.is_err();
        self.stopped
            || self
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use festerm_host_protocol::{Epoch, LeaseId};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::current_dir()
                .unwrap()
                .join(format!("hl-{}", uuid::Uuid::new_v4()));
            storage::private_directory(&root, true).unwrap();
            Self(root)
        }

        fn lease(&self, generation: u128, deadline: u128) -> GenerationId {
            let generation = GenerationId::new(generation).unwrap();
            let lease = DaemonLease {
                version: 1,
                name: "fixture".into(),
                generation,
                revision: 1,
                lifetime: Lifetime::Temporary {
                    reconnect_grace_seconds: 60,
                },
                temporary: Some(TemporaryLease {
                    lease: LeaseId::new(),
                    epoch: Epoch::new(1).unwrap(),
                    attachment: AttachmentId::new(),
                }),
                deadline_ms: Some(deadline),
                connected: false,
                stopped: false,
                remote_revoked: false,
                pending: None,
                last_attachment: None,
            };
            LeaseGuard::acquire(&self.0, generation)
                .unwrap()
                .create(&lease)
                .unwrap();
            generation
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn lease_supervisor_restart_and_busy_storage_cannot_restart_expired_grace() {
        let fixture = Fixture::new();
        fixture.lease(1, now_ms() - 1);
        assert!(DaemonWatch::open(&fixture.0, "fixture", 1)
            .unwrap()
            .should_stop());
        let generation = fixture.lease(2, now_ms() + 60_000);
        let mut watch = DaemonWatch::open(&fixture.0, "fixture", 2).unwrap();
        let _busy = LeaseGuard::acquire(&fixture.0, generation).unwrap();
        watch.deadline = Some(Instant::now());
        assert!(
            watch.should_stop(),
            "lock contention must not defeat the monotonic cap"
        );
    }

    #[test]
    fn lease_supervisor_refuses_revision_rollback_or_foreign_generation() {
        let fixture = Fixture::new();
        let generation = fixture.lease(1, now_ms() + 60_000);
        let mut watch = DaemonWatch::open(&fixture.0, "fixture", 1).unwrap();
        let guard = LeaseGuard::acquire(&fixture.0, generation).unwrap();
        let mut lease = guard.read().unwrap();
        guard.replace(&mut lease).unwrap();
        drop(guard);
        assert!(!watch.should_stop());
        let directory = storage::private_directory(&fixture.0, false).unwrap();
        lease.revision = 1;
        storage::replace_private(
            &directory,
            Path::new("host-daemon-1.json"),
            &serde_json::to_vec(&lease).unwrap(),
        )
        .unwrap();
        watch.next_poll = Instant::now();
        assert!(watch.should_stop());
        lease.generation = GenerationId::new(2).unwrap();
        storage::replace_private(
            &directory,
            Path::new("host-daemon-1.json"),
            &serde_json::to_vec(&lease).unwrap(),
        )
        .unwrap();
        assert!(DaemonWatch::open(&fixture.0, "fixture", 1).is_err());
    }
}
