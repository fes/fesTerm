//! Private generation-scoped daemon supervision. No network authority lives here.
use crate::private_storage as storage;
use festerm_host_protocol::{
    AttachmentId, Epoch, GenerationId, Lifetime, OperationId, SessionGrant, TemporaryLease,
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
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
    #[serde(with = "grant_storage")]
    pub grant: SessionGrant,
    pub writer_epoch: Epoch,
    pub native_pid: Option<u32>,
}

mod grant_storage {
    use super::*;
    use festerm_host_protocol::{LogicalSessionId, LogicalState, PrincipalId};

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Stored {
        owner: PrincipalId,
        logical: LogicalSessionId,
        revision: Epoch,
        remote_access: bool,
        state: State,
    }

    #[derive(Serialize, Deserialize)]
    enum State {
        Unknown,
        Absent,
        Running(GenerationId),
        Revoked,
    }

    pub fn serialize<S: serde::Serializer>(
        grant: &SessionGrant,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        Stored {
            owner: grant.owner.clone(),
            logical: grant.logical.clone(),
            revision: grant.revision,
            remote_access: grant.remote_access,
            state: match grant.state {
                LogicalState::Unknown => State::Unknown,
                LogicalState::Absent => State::Absent,
                LogicalState::Running(generation) => State::Running(generation),
                LogicalState::Revoked => State::Revoked,
            },
        }
        .serialize(serializer)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<SessionGrant, D::Error> {
        let stored = Stored::deserialize(deserializer)?;
        Ok(SessionGrant {
            owner: stored.owner,
            logical: stored.logical,
            revision: stored.revision,
            remote_access: stored.remote_access,
            state: match stored.state {
                State::Unknown => LogicalState::Unknown,
                State::Absent => LogicalState::Absent,
                State::Running(generation) => LogicalState::Running(generation),
                State::Revoked => LogicalState::Revoked,
            },
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingAttachment {
    pub ticket: OperationId,
    pub writer_epoch: Epoch,
    pub previous: Option<TemporaryLease>,
    pub attachment: AttachmentId,
    pub deadline_ms: u128,
}

impl DaemonLease {
    pub fn validate(&self) -> io::Result<()> {
        self.lifetime.validate().map_err(|_| invalid())?;
        if self.version != 1
            || self.revision == 0
            || self.native_pid == Some(0)
            || festerm_ssh::PersistentSessionName::new(&self.name).is_err()
        {
            return Err(invalid());
        }
        if self.grant.remote_access == self.remote_revoked
            || matches!(
                self.grant.state,
                festerm_host_protocol::LogicalState::Revoked
            ) != self.remote_revoked
            || matches!(self.grant.state, festerm_host_protocol::LogicalState::Running(generation) if generation != self.generation)
        {
            return Err(invalid());
        }
        match (self.lifetime, &self.temporary, self.deadline_ms) {
            (Lifetime::Persistent {}, None, None) => {}
            (Lifetime::Temporary { .. }, Some(_), Some(_)) => {}
            _ => return Err(invalid()),
        }
        if self.pending.as_ref().is_some_and(|pending| {
            pending.previous.as_ref() != self.temporary.as_ref()
                || pending.writer_epoch != self.writer_epoch
        }) {
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
        Self::open(root, generation, false)
    }

    pub fn reserve(root: &Path, generation: GenerationId) -> io::Result<Self> {
        Self::open(root, generation, true)
    }

    fn open(root: &Path, generation: GenerationId, create: bool) -> io::Result<Self> {
        let directory = storage::private_directory(root, false)?;
        let stem = format!("host-daemon-{}", generation.get());
        let name = format!("{stem}.lock");
        let lock = if create {
            open_lock(&directory, Path::new(&name))?
        } else {
            storage::open_private_file(&directory, Path::new(&name), false)?
        };
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
    preserve_on_fault: bool,
    binding_revocation: Cell<bool>,
    root: PathBuf,
    name: String,
    generation: GenerationId,
    revision: u64,
    deadline: Option<Instant>,
    next_poll: Instant,
    stopped: bool,
    attachment_deadline: Cell<Option<Instant>>,
    persistent_writer: Option<(OperationId, festerm_host_protocol::Epoch)>,
    disconnected_writer: Option<(OperationId, festerm_host_protocol::Epoch)>,
}

impl DaemonWatch {
    pub fn imported_remote_revoked(&self) -> bool {
        self.preserve_on_fault && self.binding_revocation.get()
    }
    pub(crate) fn seal_binding(&self) {
        if self.preserve_on_fault {
            self.binding_revocation.set(true);
        }
    }
    pub(crate) fn open_binding(root: &Path, name: &str, generation: u128) -> io::Result<Self> {
        let mut watch = Self::open(root, name, generation)?;
        watch.preserve_on_fault = true;
        Ok(watch)
    }
    fn binding_lease(&self, binding: &crate::native_binding::Binding) -> io::Result<DaemonLease> {
        let lease = LeaseGuard::acquire(&self.root, self.generation)?.read()?;
        if binding.runtime_root != self.root
            || binding.identity.name != self.name
            || binding.identity.created_at_unix_ms != self.generation.get()
            || lease.name != self.name
            || lease.native_pid != Some(binding.identity.pid)
            || lease.native_pid != Some(std::process::id())
            || lease.grant.owner != binding.principal
            || lease.grant.logical != binding.logical
            || lease.lifetime != (Lifetime::Persistent {})
            || lease.stopped
        {
            return Err(invalid());
        }
        self.observe_binding_revocation(&lease);
        Ok(lease)
    }
    fn observe_binding_revocation(&self, lease: &DaemonLease) {
        if self.preserve_on_fault && lease.remote_revoked && lease.grant.revision.get() > 1 {
            self.binding_revocation.set(true);
        }
    }

    pub(crate) fn verify_binding(
        &self,
        binding: &crate::native_binding::Binding,
    ) -> io::Result<()> {
        let lease = self.binding_lease(binding)?;
        if !lease.remote_revoked || lease.grant.revision.get() != 1 || lease.pending.is_some() {
            return Err(invalid());
        }
        Ok(())
    }

    pub(crate) fn verify_released_binding(
        &self,
        binding: &crate::native_binding::Binding,
    ) -> io::Result<()> {
        let lease = self.binding_lease(binding)?;
        if !lease.remote_revoked || lease.pending.is_some() {
            return Err(invalid());
        }
        Ok(())
    }

    pub(crate) fn binding_active(
        &self,
        binding: &crate::native_binding::Binding,
    ) -> io::Result<bool> {
        Ok(!self.binding_lease(binding)?.remote_revoked && !self.binding_revocation.get())
    }
    pub(crate) fn binding_revoked(
        &self,
        binding: &crate::native_binding::Binding,
    ) -> io::Result<bool> {
        let lease = self.binding_lease(binding)?;
        self.observe_binding_revocation(&lease);
        Ok(self.binding_revocation.get())
    }

    pub fn open(root: &Path, name: &str, generation: u128) -> io::Result<Self> {
        let generation = GenerationId::new(generation).map_err(|_| invalid())?;
        let guard = LeaseGuard::acquire(root, generation)?;
        let mut lease = guard.read()?;
        if lease.name != name || lease.generation != generation {
            return Err(invalid());
        }
        let pid = std::process::id();
        if lease.native_pid.is_some_and(|previous| previous != pid) {
            return Err(invalid());
        }
        if lease.native_pid.is_none() {
            lease.native_pid = Some(pid);
            guard.replace(&mut lease)?;
        }

        let mut watch = Self {
            preserve_on_fault: false,
            binding_revocation: Cell::new(false),
            root: root.into(),
            name: name.into(),
            generation,
            revision: 0,
            deadline: None,
            next_poll: Instant::now(),
            stopped: false,
            attachment_deadline: Cell::new(None),
            persistent_writer: None,
            disconnected_writer: None,
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
        self.persistent_writer = if lease.temporary.is_none() && lease.connected {
            lease
                .last_attachment
                .clone()
                .map(|ticket| (ticket, lease.writer_epoch))
        } else {
            None
        };
        Ok(())
    }

    /// Called only when the daemon retires its exact current ticketed frontend.
    /// Contention defers bookkeeping to the existing poll, not native admission.
    pub fn persistent_client_disconnected(&mut self) {
        if !self.preserve_on_fault && self.persistent_writer.is_some() {
            self.disconnected_writer = self.persistent_writer.clone();
            self.next_poll = Instant::now();
        }
    }

    fn reconcile_persistent_disconnect(&mut self, guard: &LeaseGuard, lease: &mut DaemonLease) {
        let Some((ticket, epoch)) = &self.disconnected_writer else {
            return;
        };
        if lease.temporary.is_some()
            || lease.last_attachment.as_ref() != Some(ticket)
            || lease.writer_epoch != *epoch
            || !lease.connected
        {
            self.disconnected_writer = None;
            return;
        }
        let mut disconnected = lease.clone();
        disconnected.connected = false;
        if guard.replace(&mut disconnected).is_ok() {
            *lease = disconnected;
            self.disconnected_writer = None;
        }
    }

    /// Validate a local admission ticket without changing lease ownership.
    pub fn allows_attachment(&self, ticket: Option<&OperationId>, remote: bool) -> bool {
        // HOST always supplies an issued ticket. Unticketed admission retains
        // the ordinary generation's pre-existing local/legacy authority path.
        if self.preserve_on_fault && ticket.is_none() {
            return true;
        }
        let Ok(guard) = LeaseGuard::acquire(&self.root, self.generation) else {
            return false;
        };
        let Ok(lease) = guard.read() else {
            return false;
        };
        self.observe_binding_revocation(&lease);
        if lease.expired() || (remote && (lease.remote_revoked || self.binding_revocation.get())) {
            return false;
        }
        match ticket {
            None => lease.temporary.is_none(),
            Some(ticket) => lease.pending.as_ref().is_some_and(|pending| {
                if &pending.ticket != ticket || pending.deadline_ms <= now_ms() {
                    return false;
                }
                self.attachment_deadline.set(Some(
                    Instant::now()
                        + Duration::from_millis(
                            pending.deadline_ms.saturating_sub(now_ms()).min(15_000) as u64,
                        ),
                ));
                true
            }),
        }
    }

    /// Contention is not cancellation; commit still performs a strict check.
    pub fn attachment_cancelled(&self, ticket: Option<&OperationId>, remote: bool) -> bool {
        if self.preserve_on_fault && ticket.is_none() {
            return false;
        }
        let guard = match LeaseGuard::acquire(&self.root, self.generation) {
            Ok(guard) => guard,
            Err(error) if crate::lock_is_contended(&error) => return false,
            Err(_) => return true,
        };
        let Ok(lease) = guard.read() else {
            return true;
        };
        self.observe_binding_revocation(&lease);
        if lease.expired() || (remote && (lease.remote_revoked || self.binding_revocation.get())) {
            return true;
        }
        match ticket {
            None => lease.temporary.is_some(),
            Some(ticket) => !lease
                .pending
                .as_ref()
                .is_some_and(|pending| &pending.ticket == ticket && pending.deadline_ms > now_ms()),
        }
    }

    /// Called by the sole daemon writer loop after recovery adoption, directly
    /// before replacing the old writer. A failed transfer preserves that writer.
    pub fn commit_attachment(
        &mut self,
        ticket: Option<&OperationId>,
        remote: bool,
    ) -> io::Result<()> {
        if self.preserve_on_fault && ticket.is_none() {
            return Ok(());
        }
        let deadline = self
            .attachment_deadline
            .get()
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(1))
            .min(Instant::now() + Duration::from_secs(1));
        let deadline = self.deadline.map_or(deadline, |lease| lease.min(deadline));
        let guard = loop {
            match LeaseGuard::acquire(&self.root, self.generation) {
                Ok(guard) => break guard,
                Err(error) if crate::lock_is_contended(&error) && Instant::now() < deadline => {
                    std::thread::sleep(
                        Duration::from_millis(5)
                            .min(deadline.saturating_duration_since(Instant::now())),
                    );
                }
                Err(error) => return Err(error),
            }
        };
        let mut lease = guard.read()?;
        self.observe_binding_revocation(&lease);
        if lease.expired() || (remote && (lease.remote_revoked || self.binding_revocation.get())) {
            return Err(invalid());
        }
        if lease.temporary.is_none() && ticket.is_none() {
            lease.writer_epoch = lease.writer_epoch.next().map_err(|_| invalid())?;
            lease.pending = None;
            lease.last_attachment = None;
            lease.connected = false;
            guard.replace(&mut lease)?;
            return self.adopt(&lease);
        }
        let pending = lease.pending.take().ok_or_else(invalid)?;
        if Some(&pending.ticket) != ticket
            || pending.deadline_ms <= now_ms()
            || lease.temporary != pending.previous
            || lease.writer_epoch != pending.writer_epoch
        {
            return Err(invalid());
        }
        if let Some(binding) = lease.temporary.as_mut() {
            binding.epoch = binding.epoch.next().map_err(|_| invalid())?;
            binding.attachment = pending.attachment;
            lease.connected = true;
            lease.deadline_ms = Some(now_ms() + 60_000);
        }
        lease.last_attachment = Some(pending.ticket);
        if lease.temporary.is_none() && !self.preserve_on_fault {
            lease.connected = true;
        }
        lease.writer_epoch = lease.writer_epoch.next().map_err(|_| invalid())?;
        guard.replace(&mut lease)?;
        self.adopt(&lease)
    }

    /// Called even while terminal output is backpressured. Any malformed or
    /// missing protected state fails closed; ordinary legacy shells never opt in.
    pub fn should_stop(&mut self) -> bool {
        if self.preserve_on_fault {
            if Instant::now() >= self.next_poll {
                self.next_poll = Instant::now() + Duration::from_millis(100);
                match LeaseGuard::acquire(&self.root, self.generation)
                    .and_then(|guard| guard.read())
                {
                    Ok(lease) => self.observe_binding_revocation(&lease),
                    Err(error) if crate::lock_is_contended(&error) => {}
                    Err(_) => self.seal_binding(),
                }
            }
            return false;
        }
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
            Ok(guard) => guard.read().and_then(|mut lease| {
                self.adopt(&lease)?;
                self.reconcile_persistent_disconnect(&guard, &mut lease);
                self.adopt(&lease)
            }),
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
                grant: SessionGrant {
                    owner: festerm_host_protocol::PrincipalId::new(),
                    logical: festerm_host_protocol::LogicalSessionId::new(),
                    revision: Epoch::new(1).unwrap(),
                    remote_access: true,
                    state: festerm_host_protocol::LogicalState::Running(generation),
                },
                writer_epoch: Epoch::new(1).unwrap(),
                native_pid: None,
            };
            LeaseGuard::reserve(&self.0, generation)
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
    fn imported_binding_fault_seals_host_access_without_stopping_or_changing_legacy_admission() {
        let fixture = Fixture::new();
        let generation = fixture.lease(1, now_ms() + 60_000);
        let guard = LeaseGuard::acquire(&fixture.0, generation).unwrap();
        let mut lease = guard.read().unwrap();
        lease.lifetime = Lifetime::Persistent {};
        lease.temporary = None;
        lease.deadline_ms = None;
        lease.remote_revoked = true;
        lease.grant.remote_access = false;
        lease.grant.state = festerm_host_protocol::LogicalState::Revoked;
        guard.replace(&mut lease).unwrap();
        drop(guard);
        let mut watch = DaemonWatch::open_binding(&fixture.0, "fixture", 1).unwrap();
        std::fs::remove_file(fixture.0.join("host-daemon-1.json")).unwrap();
        assert!(!watch.should_stop());
        assert!(watch.imported_remote_revoked());
        assert!(watch.allows_attachment(None, false));
        assert!(watch.allows_attachment(None, true));
        assert!(!watch.attachment_cancelled(None, true));
        watch.commit_attachment(None, true).unwrap();
        let ticket = OperationId::new();
        assert!(!watch.allows_attachment(Some(&ticket), true));
        assert!(watch.commit_attachment(Some(&ticket), true).is_err());
    }

    #[test]
    fn unticketed_local_attachment_requires_persistent_lifetime() {
        let fixture = Fixture::new();
        let generation = fixture.lease(1, now_ms() + 60_000);
        let mut watch = DaemonWatch::open(&fixture.0, "fixture", 1).unwrap();
        assert!(!watch.allows_attachment(None, false));
        assert!(watch.attachment_cancelled(None, false));
        assert!(watch.commit_attachment(None, false).is_err());
        let guard = LeaseGuard::acquire(&fixture.0, generation).unwrap();
        let mut lease = guard.read().unwrap();
        let original_writer = lease.writer_epoch;
        assert_eq!(lease.writer_epoch.get(), 1);
        assert!(lease.temporary.is_some());
        lease.lifetime = Lifetime::Persistent {};
        lease.temporary = None;
        lease.deadline_ms = None;
        guard.replace(&mut lease).unwrap();
        drop(guard);
        assert!(watch.allows_attachment(None, false));
        assert!(!watch.attachment_cancelled(None, false));
        watch.commit_attachment(None, false).unwrap();
        let lease = LeaseGuard::acquire(&fixture.0, generation)
            .unwrap()
            .read()
            .unwrap();
        assert_eq!(lease.writer_epoch, original_writer.next().unwrap());
        assert!(lease.temporary.is_none());
        assert!(lease.deadline_ms.is_none());
    }

    #[test]
    fn persistent_disconnect_contention_preserves_new_writer_pending_ticket_and_shell() {
        let fixture = Fixture::new();
        let generation = fixture.lease(1, now_ms() + 60_000);
        let old_ticket = OperationId::new();
        let guard = LeaseGuard::acquire(&fixture.0, generation).unwrap();
        let mut lease = guard.read().unwrap();
        lease.lifetime = Lifetime::Persistent {};
        lease.temporary = None;
        lease.deadline_ms = None;
        lease.pending = Some(PendingAttachment {
            ticket: old_ticket.clone(),
            writer_epoch: lease.writer_epoch,
            previous: None,
            attachment: AttachmentId::new(),
            deadline_ms: now_ms() + 15_000,
        });
        guard.replace(&mut lease).unwrap();
        drop(guard);
        let mut watch = DaemonWatch::open(&fixture.0, "fixture", 1).unwrap();
        assert!(watch.allows_attachment(Some(&old_ticket), true));
        watch.commit_attachment(Some(&old_ticket), true).unwrap();
        let guard = LeaseGuard::acquire(&fixture.0, generation).unwrap();
        let active = guard.read().unwrap();
        let name = Path::new("host-daemon-1.json");
        let before = storage::read_private(&guard.directory, name, 4096).unwrap();
        assert!(active.connected);
        assert!(active.pending.is_none());
        assert_eq!(active.last_attachment.as_ref(), Some(&old_ticket));
        assert_eq!(active.writer_epoch.get(), 2);
        assert!(active.deadline_ms.is_none());
        watch.persistent_client_disconnected();
        let delayed_old_disconnect = watch.disconnected_writer.clone();
        assert!(!watch.should_stop());
        assert_eq!(
            storage::read_private(&guard.directory, name, 4096).unwrap(),
            before
        );
        drop(guard);
        watch.next_poll = Instant::now();
        assert!(!watch.should_stop());
        let guard = LeaseGuard::acquire(&fixture.0, generation).unwrap();
        let mut disconnected = guard.read().unwrap();
        assert!(!disconnected.connected);
        assert!(!disconnected.stopped);
        assert!(disconnected.pending.is_none());
        assert_eq!(disconnected.writer_epoch, active.writer_epoch);
        assert_eq!(disconnected.last_attachment, active.last_attachment);
        assert!(disconnected.temporary.is_none());
        assert!(disconnected.deadline_ms.is_none());
        assert_eq!(
            serde_json::to_value(&disconnected).unwrap()["grant"],
            serde_json::to_value(&active).unwrap()["grant"]
        );
        let new_ticket = OperationId::new();
        disconnected.pending = Some(PendingAttachment {
            ticket: new_ticket.clone(),
            writer_epoch: disconnected.writer_epoch,
            previous: None,
            attachment: AttachmentId::new(),
            deadline_ms: now_ms() + 15_000,
        });
        guard.replace(&mut disconnected).unwrap();
        drop(guard);
        assert!(watch.allows_attachment(Some(&new_ticket), true));
        watch.commit_attachment(Some(&new_ticket), true).unwrap();
        let guard = LeaseGuard::acquire(&fixture.0, generation).unwrap();
        let current = guard.read().unwrap();
        let committed = storage::read_private(&guard.directory, name, 4096).unwrap();
        assert!(current.connected);
        assert_eq!(current.writer_epoch.get(), 3);
        assert_eq!(current.last_attachment.as_ref(), Some(&new_ticket));
        drop(guard);
        watch.disconnected_writer = delayed_old_disconnect;
        watch.next_poll = Instant::now();
        assert!(!watch.should_stop());
        let guard = LeaseGuard::acquire(&fixture.0, generation).unwrap();
        assert_eq!(
            storage::read_private(&guard.directory, name, 4096).unwrap(),
            committed
        );
        let mut pending = guard.read().unwrap();
        let takeover = PendingAttachment {
            ticket: OperationId::new(),
            writer_epoch: pending.writer_epoch,
            previous: None,
            attachment: AttachmentId::new(),
            deadline_ms: now_ms() + 15_000,
        };
        pending.pending = Some(takeover.clone());
        guard.replace(&mut pending).unwrap();
        drop(guard);
        watch.persistent_client_disconnected();
        watch.next_poll = Instant::now();
        assert!(!watch.should_stop());
        let guard = LeaseGuard::acquire(&fixture.0, generation).unwrap();
        let lease = guard.read().unwrap();
        assert!(!lease.connected);
        assert!(!lease.stopped);
        assert_eq!(lease.writer_epoch, pending.writer_epoch);
        assert_eq!(lease.last_attachment, pending.last_attachment);
        assert_eq!(
            serde_json::to_vec(lease.pending.as_ref().unwrap()).unwrap(),
            serde_json::to_vec(&takeover).unwrap()
        );
        assert!(lease.temporary.is_none());
        assert!(lease.deadline_ms.is_none());
        drop(guard);
        assert!(watch.allows_attachment(None, false));
        watch.commit_attachment(None, false).unwrap();
        let lease = LeaseGuard::acquire(&fixture.0, generation)
            .unwrap()
            .read()
            .unwrap();
        assert!(!lease.connected);
        assert!(lease.pending.is_none());
        assert!(lease.last_attachment.is_none());
        assert_eq!(lease.writer_epoch, pending.writer_epoch.next().unwrap());
        assert!(lease.temporary.is_none());
        assert!(lease.deadline_ms.is_none());
    }

    #[test]
    fn persistent_disconnect_bookkeeping_does_not_change_temporary_grace_or_binding_watch() {
        let fixture = Fixture::new();
        let generation = fixture.lease(1, now_ms() + 60_000);
        let ticket = OperationId::new();
        let guard = LeaseGuard::acquire(&fixture.0, generation).unwrap();
        let mut lease = guard.read().unwrap();
        lease.pending = Some(PendingAttachment {
            ticket: ticket.clone(),
            writer_epoch: lease.writer_epoch,
            previous: lease.temporary.clone(),
            attachment: AttachmentId::new(),
            deadline_ms: now_ms() + 15_000,
        });
        guard.replace(&mut lease).unwrap();
        drop(guard);
        let mut watch = DaemonWatch::open(&fixture.0, "fixture", 1).unwrap();
        assert!(watch.allows_attachment(Some(&ticket), true));
        watch.commit_attachment(Some(&ticket), true).unwrap();
        let guard = LeaseGuard::acquire(&fixture.0, generation).unwrap();
        let name = Path::new("host-daemon-1.json");
        let before = storage::read_private(&guard.directory, name, 4096).unwrap();
        drop(guard);
        watch.persistent_client_disconnected();
        watch.next_poll = Instant::now();
        assert!(!watch.should_stop());
        let guard = LeaseGuard::acquire(&fixture.0, generation).unwrap();
        assert_eq!(
            storage::read_private(&guard.directory, name, 4096).unwrap(),
            before
        );
        let mut lease = guard.read().unwrap();
        lease.lifetime = Lifetime::Persistent {};
        lease.temporary = None;
        lease.deadline_ms = None;
        lease.connected = false;
        lease.pending = Some(PendingAttachment {
            ticket: OperationId::new(),
            writer_epoch: lease.writer_epoch,
            previous: None,
            attachment: AttachmentId::new(),
            deadline_ms: now_ms() + 15_000,
        });
        let ticket = lease.pending.as_ref().unwrap().ticket.clone();
        guard.replace(&mut lease).unwrap();
        drop(guard);
        let mut watch = DaemonWatch::open_binding(&fixture.0, "fixture", 1).unwrap();
        assert!(watch.allows_attachment(Some(&ticket), true));
        watch.commit_attachment(Some(&ticket), true).unwrap();
        let guard = LeaseGuard::acquire(&fixture.0, generation).unwrap();
        let before = storage::read_private(&guard.directory, name, 4096).unwrap();
        assert!(!guard.read().unwrap().connected);
        drop(guard);
        watch.persistent_client_disconnected();
        watch.next_poll = Instant::now();
        assert!(!watch.should_stop());
        let guard = LeaseGuard::acquire(&fixture.0, generation).unwrap();
        assert_eq!(
            storage::read_private(&guard.directory, name, 4096).unwrap(),
            before
        );
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

    #[test]
    fn attachment_commit_retries_lease_contention_but_not_past_pending_deadline() {
        let fixture = Fixture::new();
        for (value, succeeds) in [(1, true), (2, false)] {
            let generation = fixture.lease(value, now_ms() + 60_000);
            let ticket = OperationId::new();
            let guard = LeaseGuard::acquire(&fixture.0, generation).unwrap();
            let mut lease = guard.read().unwrap();
            lease.pending = Some(PendingAttachment {
                ticket: ticket.clone(),
                writer_epoch: lease.writer_epoch,
                previous: lease.temporary.clone(),
                attachment: AttachmentId::new(),
                deadline_ms: now_ms() + 15_000,
            });
            guard.replace(&mut lease).unwrap();
            drop(guard);
            let mut watch = DaemonWatch::open(&fixture.0, "fixture", value).unwrap();
            assert!(watch.allows_attachment(Some(&ticket), true));
            if !succeeds {
                // Model elapsed pending grace without a filesystem-speed race.
                watch.attachment_deadline.set(Some(Instant::now()));
            }
            let guard = LeaseGuard::acquire(&fixture.0, generation).unwrap();
            let release = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(80));
                drop(guard);
            });
            let started = Instant::now();
            let outcome = watch.commit_attachment(Some(&ticket), true);
            release.join().unwrap();
            assert_eq!(outcome.is_ok(), succeeds);
            assert!(started.elapsed() < Duration::from_secs(1));
            let lease = LeaseGuard::acquire(&fixture.0, generation)
                .unwrap()
                .read()
                .unwrap();
            assert_eq!(lease.last_attachment.as_ref() == Some(&ticket), succeeds);
            assert_eq!(
                lease.temporary.unwrap().epoch.get(),
                if succeeds { 2 } else { 1 }
            );
        }
    }
}
