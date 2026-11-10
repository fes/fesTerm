//! Host-local consent journal, not a portable configuration or endpoint implementation.
//!
//! Native endpoint adapters must verify observed identities before supplying receipts.
//! No method launches a shell, registers a service, signs in, or claims readiness.

use festerm_sessiond::private_storage as storage;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{fs::File, io, path::Path};
use uuid::Uuid;

const RECORD: &str = "host-lifecycle.json";
const LOCK: &str = "host-lifecycle.lock";
const LIMIT: usize = 32 * 1024;

fn refused(stage: &'static str) -> io::Error {
    io::Error::other(stage)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    Desktop,
    Headless,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Startup {
    #[default]
    Manual,
    UserSignIn,
    BeforeLogin,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub friendly_name: String,
    #[serde(default)]
    pub allow_creation: bool,
    #[serde(default)]
    pub startup: Startup,
}

impl Settings {
    pub fn validate(&self) -> io::Result<()> {
        label(&self.friendly_name, 80)?;
        // No platform has supplied pre-login service/vault qualification yet.
        if self.startup == Startup::BeforeLogin {
            return Err(refused("host.startup_before_login_unqualified"));
        }
        Ok(())
    }
}

fn label(value: &str, max: usize) -> io::Result<()> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(refused("host.invalid_metadata"));
    }
    Ok(())
}

fn identifier(value: &str) -> io::Result<()> {
    let id = Uuid::parse_str(value).map_err(|_| refused("host.invalid_identity"))?;
    if id.get_version_num() != 4
        || id.get_variant() != uuid::Variant::RFC4122
        || id.to_string() != value
    {
        return Err(refused("host.invalid_identity"));
    }
    Ok(())
}

/// Exact, already verified cloud incarnation and protected host identity.
/// These public references contain no tokens, private keys or credentials.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub host_id: String,
    pub account: String,
    pub qualified_tunnel: String,
    pub cloud_incarnation: String,
    pub host_key_fingerprint: String,
}

impl Binding {
    fn validate(&self) -> io::Result<()> {
        identifier(&self.host_id)?;
        for value in [
            &self.account,
            &self.qualified_tunnel,
            &self.cloud_incarnation,
            &self.host_key_fingerprint,
        ] {
            label(value, 1024)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Ticket {
    installation: String,
    operation: String,
    epoch: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Disabled,
    EnableRequested,
    Provisioned,
    Reconciling,
    StopRequested,
    CleanupPending,
    Removed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    version: u16,
    pub installation: String,
    pub origin: Origin,
    pub settings: Settings,
    pub epoch: u64,
    pub phase: Phase,
    pub binding: Option<Binding>,
    pending: Option<Ticket>,
}

impl Snapshot {
    fn validate(&self, origin: Origin) -> io::Result<()> {
        if self.version != 1 || self.origin != origin {
            return Err(refused("host.foreign_origin_or_version"));
        }
        identifier(&self.installation)?;
        self.settings.validate()?;
        if let Some(binding) = &self.binding {
            binding.validate()?;
        }
        if let Some(ticket) = &self.pending {
            identifier(&ticket.operation)?;
            if ticket.installation != self.installation || ticket.epoch > self.epoch {
                return Err(refused("host.invalid_journal"));
            }
        }
        let valid = match self.phase {
            Phase::Disabled => self.pending.is_none(),
            Phase::Removed => self.pending.is_none() && self.binding.is_some(),
            Phase::EnableRequested => self.pending.as_ref().is_some_and(|t| t.epoch == self.epoch),
            Phase::Reconciling => self.pending.as_ref().is_some_and(|t| t.epoch < self.epoch),
            Phase::Provisioned | Phase::StopRequested | Phase::CleanupPending => {
                self.binding.is_some() && self.pending.is_none()
            }
        };
        if !valid {
            return Err(refused("host.invalid_journal"));
        }
        Ok(())
    }
}

/// A short per-operation exclusive lease permits revocation between endpoint calls.
pub struct Store {
    directory: File,
    origin: Origin,
}

struct Lease(File);

impl Drop for Lease {
    fn drop(&mut self) {
        // A concurrent process spawn can briefly inherit an open descriptor
        // before exec. Releasing explicitly avoids extending this lease.
        let _ = FileExt::unlock(&self.0);
    }
}

impl Store {
    /// Create a new installation. Existing journals (including other origins)
    /// are never adopted or overwritten.
    pub fn create(path: &Path, origin: Origin, settings: Settings) -> io::Result<Self> {
        settings.validate()?;
        match storage::private_directory(path, false) {
            Ok(_) => return Err(refused("host.installation_exists_or_incomplete")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let store = Self {
            directory: storage::private_directory(path, true)?,
            origin,
        };
        let lock = match storage::open_private_file(&store.directory, Path::new(LOCK), true) {
            Ok(lock) => lock,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(refused("host.installation_exists_or_incomplete"));
            }
            Err(error) => return Err(error),
        };
        lock.try_lock_exclusive()?;
        let _lease = Lease(lock);
        let record = Snapshot {
            version: 1,
            installation: Uuid::new_v4().to_string(),
            origin,
            settings,
            epoch: 0,
            phase: Phase::Disabled,
            binding: None,
            pending: None,
        };
        storage::write_new_private(
            &store.directory,
            Path::new(RECORD),
            &serde_json::to_vec(&record).map_err(|_| refused("host.invalid_journal"))?,
        )?;
        Ok(store)
    }

    pub fn open(path: &Path, origin: Origin) -> io::Result<Self> {
        let store = Self {
            directory: storage::private_directory(path, false)?,
            origin,
        };
        store.snapshot()?;
        Ok(store)
    }

    fn lease(&self) -> io::Result<Lease> {
        let lock = storage::open_private_file(&self.directory, Path::new(LOCK), false)?;
        lock.try_lock_exclusive()?;
        Ok(Lease(lock))
    }

    fn read(&self) -> io::Result<Snapshot> {
        let bytes = storage::read_private(&self.directory, Path::new(RECORD), LIMIT)?;
        let record: Snapshot =
            serde_json::from_slice(&bytes).map_err(|_| refused("host.invalid_journal"))?;
        record.validate(self.origin)?;
        Ok(record)
    }

    pub fn snapshot(&self) -> io::Result<Snapshot> {
        let _lease = self.lease()?;
        self.read()
    }

    fn change<T>(&self, update: impl FnOnce(&mut Snapshot) -> io::Result<T>) -> io::Result<T> {
        let _lease = self.lease()?;
        let mut record = self.read()?;
        let result = update(&mut record)?;
        record.validate(self.origin)?;
        storage::replace_private(
            &self.directory,
            Path::new(RECORD),
            &serde_json::to_vec(&record).map_err(|_| refused("host.invalid_journal"))?,
        )?;
        Ok(result)
    }

    /// Editing desired settings never grants consent or registers startup.
    pub fn configure(&self, settings: Settings) -> io::Result<()> {
        settings.validate()?;
        self.change(|record| {
            if record.phase != Phase::Disabled {
                return Err(refused("host.disable_before_configuration"));
            }
            record.settings = settings;
            Ok(())
        })
    }

    /// Call only after explicit foreground consent. This is not a Ready result
    /// or permission to stop/restart a legacy daemon generation.
    pub fn request_enable(&self) -> io::Result<Ticket> {
        self.change(|record| {
            if record.phase != Phase::Disabled {
                return Err(refused("host.reconcile_before_enable"));
            }
            advance(record)?;
            let ticket = Ticket {
                installation: record.installation.clone(),
                operation: Uuid::new_v4().to_string(),
                epoch: record.epoch,
            };
            record.pending = Some(ticket.clone());
            record.phase = Phase::EnableRequested;
            Ok(ticket)
        })
    }

    /// Commit only exact endpoint-adapter-verified identities. A late completion
    /// is retained for cleanup and cannot restore consent.
    pub fn record_provisioned(&self, ticket: &Ticket, binding: Binding) -> io::Result<Phase> {
        binding.validate()?;
        self.change(|record| {
            pending(record, ticket)?;
            if record.binding.as_ref().is_some_and(|old| old != &binding) {
                return Err(refused("host.identity_change_requires_migration"));
            }
            record.binding = Some(binding);
            record.pending = None;
            record.phase = if record.epoch == ticket.epoch {
                Phase::Provisioned
            } else {
                Phase::StopRequested
            };
            Ok(record.phase)
        })
    }

    /// Fence in-flight work before the caller stops remote exposure. Disabling
    /// alone never authorizes cloud deletion or ending persistent daemons.
    pub fn revoke(&self) -> io::Result<()> {
        self.change(|record| {
            if record.phase == Phase::Removed {
                return Err(refused("host.installation_removed"));
            }
            advance(record)?;
            record.phase = if record.pending.is_some() {
                Phase::Reconciling
            } else if record.binding.is_some() {
                Phase::StopRequested
            } else {
                Phase::Disabled
            };
            Ok(())
        })
    }

    /// Adapter evidence must establish that an uncertain submission created no
    /// resources. A timeout or missing reply is not sufficient.
    pub fn confirm_no_resources(&self, ticket: &Ticket) -> io::Result<()> {
        self.change(|record| {
            pending(record, ticket)?;
            if record.binding.is_some() {
                return Err(refused("host.existing_identity_requires_stop"));
            }
            record.pending = None;
            record.phase = Phase::Disabled;
            Ok(())
        })
    }

    pub fn confirm_stopped(&self, epoch: u64, exact: &Binding) -> io::Result<()> {
        self.change(|record| {
            exact_binding(record, epoch, exact)?;
            if record.phase != Phase::StopRequested {
                return Err(refused("host.stop_not_requested"));
            }
            record.phase = Phase::Disabled;
            Ok(())
        })
    }

    /// Uninstall is separate from disable and requires confirmed local stop.
    pub fn request_cleanup(&self) -> io::Result<()> {
        self.change(|record| {
            if record.phase != Phase::Disabled || record.binding.is_none() {
                return Err(refused("host.stop_before_cleanup"));
            }
            advance(record)?;
            record.phase = Phase::CleanupPending;
            Ok(())
        })
    }

    /// Keep a tombstone; startup/config import cannot reconstruct revoked consent.
    pub fn confirm_removed(&self, epoch: u64, exact: &Binding) -> io::Result<()> {
        self.change(|record| {
            exact_binding(record, epoch, exact)?;
            if record.phase != Phase::CleanupPending {
                return Err(refused("host.cleanup_not_requested"));
            }
            record.phase = Phase::Removed;
            Ok(())
        })
    }
}

fn advance(record: &mut Snapshot) -> io::Result<()> {
    record.epoch = record
        .epoch
        .checked_add(1)
        .ok_or_else(|| refused("host.epoch_exhausted"))?;
    Ok(())
}

fn pending(record: &Snapshot, ticket: &Ticket) -> io::Result<()> {
    if record.pending.as_ref() != Some(ticket) {
        return Err(refused("host.stale_operation"));
    }
    Ok(())
}

fn exact_binding(record: &Snapshot, epoch: u64, exact: &Binding) -> io::Result<()> {
    if record.epoch != epoch || record.binding.as_ref() != Some(exact) {
        return Err(refused("host.stale_or_foreign_receipt"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let store = Store::create(&root.path().join("host"), Origin::Headless, settings()).unwrap();
        (root, store)
    }

    fn settings() -> Settings {
        Settings {
            friendly_name: "Fixture host".into(),
            allow_creation: false,
            startup: Startup::Manual,
        }
    }

    fn binding() -> Binding {
        Binding {
            host_id: Uuid::new_v4().to_string(),
            account: "fixture-account".into(),
            qualified_tunnel: "fixture.region".into(),
            cloud_incarnation: "fixture-incarnation".into(),
            host_key_fingerprint: "SHA256:fixture".into(),
        }
    }

    #[test]
    fn headless_configuration_is_inert_and_origin_is_not_adopted() {
        let (root, store) = fixture();
        assert_eq!(store.snapshot().unwrap().phase, Phase::Disabled);
        assert!(!store.snapshot().unwrap().settings.allow_creation);
        assert!(Store::open(&root.path().join("host"), Origin::Desktop).is_err());
        assert!(Store::create(&root.path().join("host"), Origin::Headless, settings()).is_err());
        let unrelated = root.path().join("unrelated");
        let _directory = storage::private_directory(&unrelated, true).unwrap();
        assert!(Store::create(&unrelated, Origin::Headless, settings()).is_err());
        assert_eq!(std::fs::read_dir(unrelated).unwrap().count(), 0);
        let mut changed = settings();
        changed.startup = Startup::BeforeLogin;
        assert!(store.configure(changed).is_err());
        assert_eq!(store.snapshot().unwrap().settings, settings());
        let ticket = store.request_enable().unwrap();
        assert!(store.configure(settings()).is_err());
        assert!(store.request_enable().is_err());
        store.confirm_no_resources(&ticket).unwrap();
        store.configure(settings()).unwrap();
    }

    #[test]
    fn cancelled_setup_journals_late_exact_identity_without_restoring_consent() {
        let (root, store) = fixture();
        let ticket = store.request_enable().unwrap();
        store.revoke().unwrap();
        assert_eq!(store.snapshot().unwrap().phase, Phase::Reconciling);
        assert!(store.request_enable().is_err());
        let exact = binding();
        assert_eq!(
            store.record_provisioned(&ticket, exact.clone()).unwrap(),
            Phase::StopRequested
        );
        drop(store);
        let store = Store::open(&root.path().join("host"), Origin::Headless).unwrap();
        let snapshot = store.snapshot().unwrap();
        assert!(store.record_provisioned(&ticket, exact.clone()).is_err());
        assert!(store.confirm_removed(snapshot.epoch, &binding()).is_err());
        assert!(store.confirm_removed(snapshot.epoch, &exact).is_err());
        store.confirm_stopped(snapshot.epoch, &exact).unwrap();
        store.request_cleanup().unwrap();
        store
            .confirm_removed(store.snapshot().unwrap().epoch, &exact)
            .unwrap();
        assert!(store.request_enable().is_err());
        assert_eq!(store.snapshot().unwrap().binding, Some(exact));
    }

    #[test]
    fn disabled_reenable_preserves_identity_and_cleanup_requires_exact_stop() {
        let (_root, store) = fixture();
        let ticket = store.request_enable().unwrap();
        let exact = binding();
        store.record_provisioned(&ticket, exact.clone()).unwrap();
        assert!(store.request_cleanup().is_err());
        store.revoke().unwrap();
        let epoch = store.snapshot().unwrap().epoch;
        assert!(store.confirm_stopped(epoch - 1, &exact).is_err());
        store.confirm_stopped(epoch, &exact).unwrap();
        let retry = store.request_enable().unwrap();
        assert!(store.record_provisioned(&retry, binding()).is_err());
        store.record_provisioned(&retry, exact.clone()).unwrap();
        store.revoke().unwrap();
        store
            .confirm_stopped(store.snapshot().unwrap().epoch, &exact)
            .unwrap();
        store.request_cleanup().unwrap();
        let epoch = store.snapshot().unwrap().epoch;
        assert!(store.confirm_removed(epoch, &binding()).is_err());
        store.confirm_removed(epoch, &exact).unwrap();
        assert_eq!(store.snapshot().unwrap().phase, Phase::Removed);
    }

    #[test]
    fn host_journal_refuses_busy_corrupt_and_unprotected_state() {
        let (root, store) = fixture();
        let lease = store.lease().unwrap();
        assert_eq!(
            store.request_enable().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(lease);
        let path = root.path().join("host");
        storage::replace_private(&store.directory, Path::new(RECORD), b"{}").unwrap();
        assert!(Store::open(&path, Origin::Headless).is_err());
        assert!(Store::create(&path, Origin::Headless, settings()).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::{symlink, PermissionsExt};
            let link = root.path().join("link");
            symlink(&path, &link).unwrap();
            assert!(Store::open(&link, Origin::Headless).is_err());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(Store::open(&path, Origin::Headless).is_err());
        }
    }
}
