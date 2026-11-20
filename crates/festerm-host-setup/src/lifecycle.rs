//! Host-local consent journal, not a portable configuration or endpoint implementation.
//!
//! Native endpoint adapters must verify observed identities before supplying receipts.
//! No method launches a shell, registers a service, signs in, or claims readiness.

use crate::protected_state::{self, Lease};
use festerm_sessiond::private_storage as storage;
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
    /// Protected Enable consent; absent legacy fields never grant ordinary access.
    #[serde(default, skip_serializing_if = "OrdinaryAccess::is_owner_only")]
    pub ordinary_access: OrdinaryAccess,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem: Option<festerm_tunnel_bridge::file_protocol::Policy>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OrdinaryAccess {
    #[default]
    OwnerOnly,
    ProviderOwner,
}

impl OrdinaryAccess {
    fn is_owner_only(&self) -> bool {
        *self == Self::OwnerOnly
    }
}

impl Settings {
    pub fn validate(&self) -> io::Result<()> {
        label(&self.friendly_name, 80)?;
        if let Some(policy) = &self.filesystem {
            if self.ordinary_access != OrdinaryAccess::ProviderOwner {
                return Err(refused(
                    "File access requires provider-authorized remote access.",
                ));
            }
            policy
                .validate()
                .map_err(|_| refused("Choose an absolute local folder for file access."))?;
        }
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
    approved_settings: Settings,
}

impl Ticket {
    pub fn approved_settings(&self) -> &Settings {
        &self.approved_settings
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }
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
    /// Recover the exact protected in-flight operation after interruption.
    /// Absence is not permission to reconstruct consent from configuration.
    pub fn pending_ticket(&self) -> Option<&Ticket> {
        self.pending.as_ref()
    }

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
            ticket.approved_settings.validate()?;
            if ticket.approved_settings != self.settings {
                return Err(refused("host.settings_changed_reconfirm"));
            }
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

impl Store {
    #[cfg(feature = "owner-enrollment")]
    pub(crate) fn with_owned_lease<T>(
        &self,
        operation: impl FnOnce(&File, &Snapshot, Lease) -> io::Result<T>,
    ) -> io::Result<T> {
        let lease = self.lease()?;
        operation(&self.directory, &self.read()?, lease)
    }

    pub(crate) fn with_locked<T>(
        &self,
        operation: impl FnOnce(&File, &Snapshot) -> io::Result<T>,
    ) -> io::Result<T> {
        let _lease = self.lease()?;
        operation(&self.directory, &self.read()?)
    }

    /// Create a new installation. Existing journals (including other origins)
    /// are never adopted or overwritten.
    pub fn create(path: &Path, origin: Origin, settings: Settings) -> io::Result<Self> {
        settings.validate()?;
        match storage::private_directory(path, false) {
            Ok(directory) => {
                return match storage::open_private_file(&directory, Path::new(RECORD), false) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        Err(refused("host.recordless_directory_requires_manual_review"))
                    }
                    Err(error) => Err(error),
                    Ok(_) => Err(refused("host.installation_exists_or_incomplete")),
                };
            }
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
        let _lease = Lease::exclusive(lock)?;
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
        protected_state::publish_new(
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
        let lock = storage::open_private_file(&self.directory, Path::new(LOCK), false).map_err(
            |error| {
                if error.kind() == io::ErrorKind::NotFound {
                    match storage::open_private_file(&self.directory, Path::new(RECORD), false) {
                        Ok(_) => refused("host.journal_lock_missing_reconciliation_required"),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {
                            refused("host.recordless_directory_requires_manual_review")
                        }
                        Err(error) => error,
                    }
                } else {
                    error
                }
            },
        )?;
        Lease::exclusive(lock)
    }

    fn read(&self) -> io::Result<Snapshot> {
        let bytes =
            storage::read_private(&self.directory, Path::new(RECORD), LIMIT).map_err(|error| {
                if error.kind() == io::ErrorKind::NotFound {
                    refused("host.recordless_directory_requires_manual_review")
                } else {
                    error
                }
            })?;
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

    /// Supply the exact settings shown and approved in the foreground, not a
    /// fresh configuration read. Changed settings require renewed confirmation.
    /// This is not Ready or permission to stop/restart a legacy generation.
    pub fn request_enable(&self, approved_settings: &Settings) -> io::Result<Ticket> {
        self.request_enable_current(None, approved_settings, false)
    }

    /// Atomically bind foreground consent and edited settings to the exact
    /// protected observation, before any sign-in or native effect.
    pub fn request_enable_if_current(
        &self,
        expected: &Snapshot,
        approved_settings: &Settings,
    ) -> io::Result<Ticket> {
        self.request_enable_current(Some(expected), approved_settings, false)
    }

    /// Complete Desktop foreground review, including the private file folder
    /// and permissions. Apply only to this exact disabled Desktop installation
    /// and reserve its Enable ticket in the same protected transaction.
    pub fn request_desktop_enable_if_current(
        &self,
        expected: &Snapshot,
        approved_settings: &Settings,
    ) -> io::Result<Ticket> {
        self.request_enable_current(Some(expected), approved_settings, true)
    }

    fn request_enable_current(
        &self,
        expected: Option<&Snapshot>,
        approved_settings: &Settings,
        desktop_file_review: bool,
    ) -> io::Result<Ticket> {
        approved_settings.validate()?;
        self.change(|record| {
            if desktop_file_review && (record.origin != Origin::Desktop || expected.is_none()) {
                return Err(refused("host.desktop_consent_required"));
            }
            if expected.is_some_and(|expected| expected != record) {
                return Err(refused("host.foreground_scope_changed"));
            }
            crate::owned_migration::admit_enable(&self.directory, record)?;
            if record.phase != Phase::Disabled {
                return Err(refused("host.reconcile_before_enable"));
            }
            if !desktop_file_review && record.settings.filesystem != approved_settings.filesystem {
                return Err(refused("File access changed. Review the file folder and permissions while remote access is disabled, then enable again."));
            }
            if expected.is_some() {
                record.settings = approved_settings.clone();
            }
            if &record.settings != approved_settings {
                return Err(refused("host.settings_changed_reconfirm"));
            }
            advance(record)?;
            let ticket = Ticket {
                installation: record.installation.clone(),
                operation: Uuid::new_v4().to_string(),
                epoch: record.epoch,
                approved_settings: approved_settings.clone(),
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
        self.revoke_current(None).map(|_| ())
    }

    /// An old desktop confirmation cannot revoke a replacement epoch.
    pub fn revoke_if_current(&self, expected: &Snapshot) -> io::Result<Snapshot> {
        self.revoke_current(Some(expected))
    }

    fn revoke_current(&self, expected: Option<&Snapshot>) -> io::Result<Snapshot> {
        self.change(|record| {
            if expected.is_some_and(|expected| expected != record) {
                return Err(refused("host.foreground_scope_changed"));
            }
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
            Ok(record.clone())
        })
    }

    #[cfg(feature = "cloud")]
    pub(crate) fn cancel_enable(&self, ticket: &Ticket) -> io::Result<()> {
        self.change(|record| {
            if record.installation != ticket.installation || record.epoch != ticket.epoch {
                return Ok(());
            }
            if record.pending.as_ref() != Some(ticket) && record.phase != Phase::Provisioned {
                return Ok(());
            }
            advance(record)?;
            record.phase = if record.pending.is_some() {
                Phase::Reconciling
            } else {
                Phase::StopRequested
            };
            Ok(())
        })
    }

    #[cfg(feature = "cloud")]
    pub(crate) fn release_uncertain_enable(&self, ticket: &Ticket, epoch: u64) -> io::Result<()> {
        self.change(|record| {
            pending(record, ticket)?;
            if record.epoch != epoch
                || record.epoch <= ticket.epoch
                || record.phase != Phase::Reconciling
                || record.binding.is_some()
            {
                return Err(refused("host.cloud_uncertain_recovery_unavailable"));
            }
            advance(record)?;
            record.pending = None;
            record.phase = Phase::Disabled;
            Ok(())
        })
    }

    /// A cloud absence is not native stop evidence. Retain the exact binding,
    /// fence enable intent and require the ordinary exact native-stop receipt.
    #[cfg(feature = "cloud")]
    pub(crate) fn record_missing_resource(
        &self,
        ticket: &Ticket,
        binding: Binding,
    ) -> io::Result<Phase> {
        binding.validate()?;
        self.change(|record| {
            if record.installation != ticket.installation {
                return Err(refused("host.stale_operation"));
            }
            if record.binding.as_ref().is_some_and(|old| old != &binding) {
                return Err(refused("host.identity_change_requires_migration"));
            }
            if record.pending.is_none() && record.binding.as_ref() == Some(&binding) {
                if record.phase != Phase::Provisioned {
                    return Ok(record.phase);
                }
                if record.epoch != ticket.epoch {
                    return Err(refused("host.stale_operation"));
                }
            } else {
                pending(record, ticket)?;
            }
            if record.epoch == ticket.epoch {
                advance(record)?;
            }
            record.binding = Some(binding);
            record.pending = None;
            record.phase = Phase::StopRequested;
            Ok(record.phase)
        })
    }

    /// Adapter evidence must establish that an uncertain submission created no
    /// resources. A timeout or missing reply is not sufficient.
    pub fn confirm_no_resources(&self, ticket: &Ticket) -> io::Result<()> {
        self.confirm_no_resources_current(None, ticket)
    }

    #[cfg(feature = "cloud")]
    pub(crate) fn confirm_no_resources_if_current(
        &self,
        expected: &Snapshot,
        ticket: &Ticket,
    ) -> io::Result<()> {
        self.confirm_no_resources_current(Some(expected), ticket)
    }

    fn confirm_no_resources_current(
        &self,
        expected: Option<&Snapshot>,
        ticket: &Ticket,
    ) -> io::Result<()> {
        self.change(|record| {
            if expected.is_some_and(|expected| expected != record) {
                return Err(refused("host.foreground_scope_changed"));
            }
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
        self.confirm_stopped_snapshot(epoch, exact).map(|_| ())
    }

    pub(crate) fn confirm_stopped_snapshot(
        &self,
        epoch: u64,
        exact: &Binding,
    ) -> io::Result<Snapshot> {
        self.change(|record| {
            exact_binding(record, epoch, exact)?;
            if record.phase != Phase::StopRequested {
                return Err(refused("host.stop_not_requested"));
            }
            record.phase = Phase::Disabled;
            Ok(record.clone())
        })
    }

    #[cfg(feature = "cloud")]
    pub(crate) fn adopt_for_cleanup(
        &self,
        ticket: &Ticket,
        expected_epoch: u64,
        binding: Binding,
    ) -> io::Result<Phase> {
        binding.validate()?;
        self.change(|record| {
            if record.phase != Phase::Disabled
                || record.epoch != expected_epoch
                || record.epoch <= ticket.epoch
                || record.installation != ticket.installation
                || record.binding.is_some()
                || record.pending.is_some()
            {
                return Err(refused("host.cleanup_adoption_scope_changed"));
            }
            advance(record)?;
            record.binding = Some(binding);
            record.phase = Phase::StopRequested;
            Ok(record.phase)
        })
    }

    /// Uninstall is separate from disable and requires confirmed local stop.
    pub fn request_cleanup(&self) -> io::Result<()> {
        self.request_cleanup_current(None).map(|_| ())
    }

    #[cfg(all(
        feature = "cloud",
        feature = "host-runtime",
        feature = "native-services"
    ))]
    pub(crate) fn request_cleanup_if_current(&self, expected: &Snapshot) -> io::Result<Snapshot> {
        self.request_cleanup_current(Some(expected))
    }

    fn request_cleanup_current(&self, expected: Option<&Snapshot>) -> io::Result<Snapshot> {
        self.change(|record| {
            if expected.is_some_and(|expected| expected != record) {
                return Err(refused("host.foreground_scope_changed"));
            }
            if record.phase != Phase::Disabled || record.binding.is_none() {
                return Err(refused("host.stop_before_cleanup"));
            }
            advance(record)?;
            record.phase = Phase::CleanupPending;
            Ok(record.clone())
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

    fn fixture() -> (storage::PrivateTestDirectory, Store) {
        let root = storage::private_test_directory();
        let store = Store::create(&root.path().join("host"), Origin::Headless, settings()).unwrap();
        (root, store)
    }

    #[cfg(feature = "cloud")]
    #[test]
    fn host_cloud_absence_release_requires_exact_epoch_even_when_old_ticket_survives() {
        let (_root, store) = fixture();
        let ticket = store.request_enable(&settings()).unwrap();
        let observed = store.snapshot().unwrap();
        store.revoke().unwrap();
        let advanced = store.snapshot().unwrap();
        assert_eq!(advanced.pending_ticket(), Some(&ticket));
        assert!(store
            .confirm_no_resources_if_current(&observed, &ticket)
            .is_err());
        assert_eq!(store.snapshot().unwrap(), advanced);
        store
            .confirm_no_resources_if_current(&advanced, &ticket)
            .unwrap();
        assert_eq!(store.snapshot().unwrap().phase, Phase::Disabled);
    }

    #[test]
    fn host_desktop_consent_atomically_binds_settings_and_exact_snapshot() {
        let (_root, store) = fixture();
        let observed = store.snapshot().unwrap();
        let approved = Settings {
            friendly_name: "Explicit desktop consent".into(),
            allow_creation: true,
            ..settings()
        };
        let ticket = store
            .request_enable_if_current(&observed, &approved)
            .unwrap();
        assert_eq!(ticket.approved_settings(), &approved);
        assert_eq!(store.snapshot().unwrap().settings, approved);
        assert_eq!(store.snapshot().unwrap().phase, Phase::EnableRequested);
        let current = store.snapshot().unwrap();
        assert!(store
            .request_enable_if_current(&observed, &settings())
            .is_err());
        assert!(store.revoke_if_current(&observed).is_err());
        assert_eq!(store.snapshot().unwrap(), current);
    }

    #[test]
    fn host_desktop_file_review_reserves_once_and_retains_revoked_late_completion() {
        let root = storage::private_test_directory();
        let store =
            Store::create(&root.path().join("desktop"), Origin::Desktop, settings()).unwrap();
        let original = store.snapshot().unwrap();
        let mut approved = original.settings.clone();
        approved.ordinary_access = OrdinaryAccess::ProviderOwner;
        approved.filesystem = Some(festerm_tunnel_bridge::file_protocol::Policy {
            root: root.path().into(),
            write: true,
        });
        let ticket = store
            .request_desktop_enable_if_current(&original, &approved)
            .unwrap();
        let reserved = store.snapshot().unwrap();
        assert_eq!(reserved.epoch, original.epoch + 1);
        assert_eq!(reserved.settings, approved);
        assert_eq!(reserved.pending_ticket(), Some(&ticket));
        assert_eq!(ticket.approved_settings(), &approved);
        assert!(store
            .request_desktop_enable_if_current(&original, &approved)
            .is_err());
        assert!(store
            .request_desktop_enable_if_current(&reserved, &approved)
            .is_err());
        assert_eq!(store.snapshot().unwrap(), reserved);
        store.revoke().unwrap();
        assert_eq!(
            store.record_provisioned(&ticket, binding()).unwrap(),
            Phase::StopRequested
        );
        let stopped = store.snapshot().unwrap();
        assert_eq!(stopped.settings.filesystem, approved.filesystem);
        store
            .confirm_stopped(stopped.epoch, stopped.binding.as_ref().unwrap())
            .unwrap();
        let disabled = store.snapshot().unwrap();
        approved.filesystem.as_mut().unwrap().write = false;
        let next = store
            .request_desktop_enable_if_current(&disabled, &approved)
            .unwrap();
        assert_eq!(next.epoch(), disabled.epoch + 1);
        assert_eq!(next.approved_settings(), &approved);
    }

    #[test]
    fn host_desktop_file_review_rejects_headless_stale_and_invalid_without_mutation() {
        for origin in [Origin::Headless, Origin::Desktop] {
            let root = storage::private_test_directory();
            let store = Store::create(&root.path().join("host"), origin, settings()).unwrap();
            let original = store.snapshot().unwrap();
            let mut approved = original.settings.clone();
            approved.ordinary_access = OrdinaryAccess::ProviderOwner;
            approved.filesystem = Some(festerm_tunnel_bridge::file_protocol::Policy {
                root: root.path().into(),
                write: false,
            });
            if origin == Origin::Headless {
                assert!(store
                    .request_desktop_enable_if_current(&original, &approved)
                    .is_err());
            } else {
                let mut stale = original.clone();
                stale.settings.friendly_name = "Unobserved editor".into();
                assert!(store
                    .request_desktop_enable_if_current(&stale, &approved)
                    .is_err());
                approved.ordinary_access = OrdinaryAccess::OwnerOnly;
                assert!(store
                    .request_desktop_enable_if_current(&original, &approved)
                    .is_err());
            }
            assert_eq!(store.snapshot().unwrap(), original);
        }
    }

    #[test]
    fn host_desktop_consent_refuses_same_phase_epoch_and_same_epoch_metadata_drift() {
        for metadata in [false, true] {
            let (_root, store) = fixture();
            let observed = store.snapshot().unwrap();
            if metadata {
                store
                    .configure(Settings {
                        friendly_name: "Another foreground editor".into(),
                        ..settings()
                    })
                    .unwrap();
            } else {
                store.revoke().unwrap();
            }
            let current = store.snapshot().unwrap();
            assert_eq!(current.phase, observed.phase);
            assert!(store
                .request_enable_if_current(&observed, &settings())
                .is_err());
            assert!(store.revoke_if_current(&observed).is_err());
            assert_eq!(store.snapshot().unwrap(), current);
        }
    }

    #[test]
    fn host_desktop_consent_busy_and_foreign_scope_never_mutate() {
        let (_root, store) = fixture();
        let original = store.snapshot().unwrap();
        let mut foreign = original.clone();
        foreign.installation = Uuid::new_v4().to_string();
        assert!(store
            .request_enable_if_current(&foreign, &settings())
            .is_err());
        assert!(store.revoke_if_current(&foreign).is_err());
        let lease = store.lease().unwrap();
        assert_eq!(
            store
                .request_enable_if_current(&original, &settings())
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(
            store.revoke_if_current(&original).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(lease);
        assert_eq!(store.snapshot().unwrap(), original);
    }

    fn settings() -> Settings {
        Settings {
            filesystem: None,
            ordinary_access: Default::default(),
            friendly_name: "Fixture host".into(),
            allow_creation: false,
            startup: Startup::Manual,
        }
    }

    #[test]
    fn host_files_policy_is_default_off_strict_and_bound_to_reviewed_settings() {
        let (_root, store) = fixture();
        let original = store.snapshot().unwrap();
        let mut requested = original.settings.clone();
        requested.ordinary_access = OrdinaryAccess::ProviderOwner;
        requested.filesystem = Some(festerm_tunnel_bridge::file_protocol::Policy {
            root: std::env::temp_dir().join("owned-files-review"),
            write: false,
        });
        let mut wrong_mode = requested.clone();
        wrong_mode.ordinary_access = OrdinaryAccess::OwnerOnly;
        assert!(store.configure(wrong_mode).is_err());
        assert!(
            store
                .request_enable_if_current(&original, &requested)
                .is_err(),
            "new file grants require explicit Disabled configuration"
        );
        store.configure(requested.clone()).unwrap();
        let configured = store.snapshot().unwrap();
        let mut old_surface = requested.clone();
        old_surface.filesystem = None;
        assert!(store
            .request_enable_if_current(&configured, &old_surface)
            .is_err());
        assert_eq!(store.snapshot().unwrap(), configured);
        let ticket = store
            .request_enable_if_current(&configured, &requested)
            .unwrap();
        assert_eq!(ticket.approved_settings(), &requested);
        let current = store.snapshot().unwrap();
        assert!(store.configure(old_surface).is_err());
        assert_eq!(store.snapshot().unwrap(), current);
        store.record_provisioned(&ticket, binding()).unwrap();
        store.revoke().unwrap();
        let stopped = store.snapshot().unwrap();
        assert_eq!(stopped.settings.filesystem, requested.filesystem);
        store
            .confirm_stopped(stopped.epoch, stopped.binding.as_ref().unwrap())
            .unwrap();
        let mut disabled = store.snapshot().unwrap().settings;
        disabled.filesystem = None;
        store.configure(disabled).unwrap();
        assert!(store.snapshot().unwrap().settings.filesystem.is_none());
        let mut malformed = serde_json::to_value(requested).unwrap();
        malformed["filesystem"]["extra"] = true.into();
        assert!(serde_json::from_value::<Settings>(malformed).is_err());
        let mut relative = configured.settings;
        relative.filesystem.as_mut().unwrap().root = "relative".into();
        assert!(relative.validate().is_err());
        let mut legacy = serde_json::to_value(original.settings).unwrap();
        assert!(legacy.get("filesystem").is_none());
        legacy["filesystem"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<Settings>(legacy)
            .unwrap()
            .filesystem
            .is_none());
    }

    #[test]
    fn ordinary_policy_missing_remains_owner_only_without_rewriting_legacy_settings() {
        let legacy = serde_json::json!({
            "friendly_name": "Legacy host", "allow_creation": false, "startup": "manual"
        });
        let settings: Settings = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(settings.ordinary_access, OrdinaryAccess::OwnerOnly);
        assert_eq!(serde_json::to_value(settings).unwrap(), legacy);
    }

    #[test]
    fn provider_owner_requires_disabled_reviewed_enable_and_cannot_retarget_consent() {
        let (_root, store) = fixture();
        let original = store.snapshot().unwrap();
        let mut requested = serde_json::to_value(&original.settings).unwrap();
        requested["ordinary_access"] = "provider-owner".into();
        let requested: Settings = serde_json::from_value(requested).unwrap();
        // Merely decoding a requested mode cannot grant access or mutate this store.
        assert_eq!(store.snapshot().unwrap(), original);
        store.configure(requested.clone()).unwrap();
        assert_eq!(store.snapshot().unwrap().phase, Phase::Disabled);
        assert!(store.request_enable(&original.settings).is_err());
        let ticket = store.request_enable(&requested).unwrap();
        assert_eq!(ticket.approved_settings(), &requested);
        let requested_snapshot = store.snapshot().unwrap();
        assert!(store.configure(original.settings).is_err());
        assert_eq!(store.snapshot().unwrap(), requested_snapshot);
    }

    #[test]
    fn legacy_reader_refuses_explicit_provider_owner_but_keeps_owner_only_compatible() {
        #[derive(Deserialize, Serialize)]
        #[serde(deny_unknown_fields)]
        struct LegacySettings {
            friendly_name: String,
            allow_creation: bool,
            startup: Startup,
        }
        let owner_only = serde_json::to_value(settings()).unwrap();
        assert!(serde_json::from_value::<LegacySettings>(owner_only.clone()).is_ok());
        let mut provider = owner_only;
        provider["ordinary_access"] = "provider-owner".into();
        assert!(serde_json::from_value::<Settings>(provider.clone()).is_ok());
        assert!(serde_json::from_value::<LegacySettings>(provider).is_err());
    }

    #[test]
    fn ordinary_policy_unknown_values_never_become_provider_access() {
        for mode in ["", "provider", "anonymous", "provider-owner-v2"] {
            let mut requested = serde_json::to_value(settings()).unwrap();
            requested["ordinary_access"] = mode.into();
            assert!(serde_json::from_value::<Settings>(requested).is_err());
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
        let ticket = store.request_enable(&settings()).unwrap();
        assert!(store.configure(settings()).is_err());
        assert!(store.request_enable(&settings()).is_err());
        store.confirm_no_resources(&ticket).unwrap();
        store.configure(settings()).unwrap();
    }

    #[test]
    fn cancelled_setup_journals_late_exact_identity_without_restoring_consent() {
        let (root, store) = fixture();
        let original = store.request_enable(&settings()).unwrap();
        drop(store);
        let store = Store::open(&root.path().join("host"), Origin::Headless).unwrap();
        let ticket = store.snapshot().unwrap().pending_ticket().cloned().unwrap();
        assert_eq!(ticket, original);
        store.revoke().unwrap();
        assert_eq!(store.snapshot().unwrap().phase, Phase::Reconciling);
        assert_eq!(store.snapshot().unwrap().epoch, ticket.epoch() + 1);
        assert!(store.request_enable(&settings()).is_err());
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
        assert!(store.request_enable(&settings()).is_err());
        assert_eq!(store.snapshot().unwrap().binding, Some(exact));
    }

    #[test]
    fn disabled_reenable_preserves_identity_and_cleanup_requires_exact_stop() {
        let (_root, store) = fixture();
        let ticket = store.request_enable(&settings()).unwrap();
        let exact = binding();
        store.record_provisioned(&ticket, exact.clone()).unwrap();
        assert!(store.request_cleanup().is_err());
        store.revoke().unwrap();
        let epoch = store.snapshot().unwrap().epoch;
        assert!(store.confirm_stopped(epoch - 1, &exact).is_err());
        store.confirm_stopped(epoch, &exact).unwrap();
        let retry = store.request_enable(&settings()).unwrap();
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
        use fs2::FileExt;

        let (root, store) = fixture();
        let lease = store.lease().unwrap();
        let competing =
            storage::open_private_file(&store.directory, Path::new(LOCK), false).unwrap();
        assert_eq!(
            competing.try_lock_exclusive().unwrap_err().raw_os_error(),
            fs2::lock_contended_error().raw_os_error()
        );
        assert_eq!(
            store.request_enable(&settings()).unwrap_err().kind(),
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

    #[test]
    fn interrupted_enable_recovers_exact_ticket_to_confirm_no_resources() {
        let (root, store) = fixture();
        let original = store.request_enable(&settings()).unwrap();
        drop(store);
        let store = Store::open(&root.path().join("host"), Origin::Headless).unwrap();
        let snapshot = store.snapshot().unwrap();
        let recovered = snapshot.pending_ticket().unwrap();
        assert_eq!(recovered, &original);
        assert_eq!(recovered.epoch(), snapshot.epoch);
        store.confirm_no_resources(recovered).unwrap();
        let disabled = store.snapshot().unwrap();
        assert_eq!(disabled.phase, Phase::Disabled);
        assert!(disabled.pending_ticket().is_none());
        assert!(disabled.binding.is_none());
    }

    #[test]
    fn recordless_initialization_requires_manual_review_without_adopting_or_replacing_evidence() {
        let root = storage::private_test_directory();
        let path = root.path().join("host");
        let directory = storage::private_directory(&path, true).unwrap();
        assert_eq!(
            Store::open(&path, Origin::Headless)
                .err()
                .unwrap()
                .to_string(),
            "host.recordless_directory_requires_manual_review"
        );
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
        storage::write_new_private(&directory, Path::new(LOCK), b"").unwrap();
        assert_eq!(
            Store::open(&path, Origin::Headless)
                .err()
                .unwrap()
                .to_string(),
            "host.recordless_directory_requires_manual_review"
        );
        storage::write_new_private(&directory, Path::new(".stage-interrupted"), b"partial")
            .unwrap();
        let lease = Lease::exclusive(
            storage::open_private_file(&directory, Path::new(LOCK), false).unwrap(),
        )
        .unwrap();
        let busy = Store::open(&path, Origin::Headless).err().unwrap();
        assert_eq!(busy.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(busy.to_string(), "host.installation_busy");
        drop(lease);
        assert_eq!(
            Store::open(&path, Origin::Headless)
                .err()
                .unwrap()
                .to_string(),
            "host.recordless_directory_requires_manual_review"
        );
        assert_eq!(
            Store::create(&path, Origin::Headless, settings())
                .err()
                .unwrap()
                .to_string(),
            "host.recordless_directory_requires_manual_review"
        );
        assert!(!path.join(RECORD).exists());
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 2);
        storage::write_new_private(&directory, Path::new(RECORD), b"{").unwrap();
        assert_eq!(
            Store::open(&path, Origin::Headless)
                .err()
                .unwrap()
                .to_string(),
            "host.invalid_journal"
        );
        std::fs::remove_file(path.join(LOCK)).unwrap();
        assert_eq!(
            Store::open(&path, Origin::Headless)
                .err()
                .unwrap()
                .to_string(),
            "host.journal_lock_missing_reconciliation_required"
        );
        assert!(!path.join(LOCK).exists());
        assert_eq!(
            storage::read_private(&directory, Path::new(RECORD), LIMIT)
                .unwrap()
                .as_slice(),
            b"{"
        );
        let healthy = root.path().join("healthy");
        let created = Store::create(&healthy, Origin::Headless, settings()).unwrap();
        let reopened = Store::open(&healthy, Origin::Headless).unwrap();
        assert_eq!(
            reopened.snapshot().unwrap().installation,
            created.snapshot().unwrap().installation
        );
        assert_eq!(reopened.snapshot().unwrap().phase, Phase::Disabled);
    }

    #[test]
    fn host_enable_requires_exact_foreground_settings_and_binds_the_ticket() {
        for changed in [
            Settings {
                allow_creation: true,
                ..settings()
            },
            Settings {
                startup: Startup::UserSignIn,
                ..settings()
            },
        ] {
            let (root, store) = fixture();
            let prompted = store.snapshot().unwrap().settings;
            store.configure(changed.clone()).unwrap();
            let before = store.snapshot().unwrap();
            assert_eq!(
                store.request_enable(&prompted).unwrap_err().to_string(),
                "host.settings_changed_reconfirm"
            );
            assert_eq!(store.snapshot().unwrap(), before);
            let ticket = store.request_enable(&changed).unwrap();
            assert_eq!(ticket.approved_settings(), &changed);
            drop(store);
            let store = Store::open(&root.path().join("host"), Origin::Headless).unwrap();
            let mut forged = ticket.clone();
            forged.approved_settings = prompted;
            assert!(store.record_provisioned(&forged, binding()).is_err());
            assert!(store.snapshot().unwrap().binding.is_none());
            assert_eq!(
                store.record_provisioned(&ticket, binding()).unwrap(),
                Phase::Provisioned
            );
        }
    }

    #[test]
    fn host_provisioning_refuses_settings_that_drift_from_the_approved_ticket() {
        let (_root, store) = fixture();
        let ticket = store.request_enable(&settings()).unwrap();
        let mut drifted = store.snapshot().unwrap();
        drifted.settings.allow_creation = true;
        storage::replace_private(
            &store.directory,
            Path::new(RECORD),
            &serde_json::to_vec(&drifted).unwrap(),
        )
        .unwrap();
        assert_eq!(
            store
                .record_provisioned(&ticket, binding())
                .unwrap_err()
                .to_string(),
            "host.settings_changed_reconfirm"
        );
        let bytes = storage::read_private(&store.directory, Path::new(RECORD), LIMIT).unwrap();
        let unchanged: Snapshot = serde_json::from_slice(&bytes).unwrap();
        assert!(unchanged.binding.is_none());
        assert_eq!(unchanged.phase, Phase::EnableRequested);
    }
}
