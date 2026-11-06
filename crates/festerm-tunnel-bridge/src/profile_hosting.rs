//! Protected desktop hosting consent. Configuration alone never authorizes a
//! resource creation; revocation fences operations already submitted to the cloud.

use crate::setup_storage;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

const RECORD: &str = "profile-consent.json";
const LOCK: &str = "profile-consent.lock";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagementRequest {
    pub version: u16,
    pub operation: ManagementOperation,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ManagementOperation {
    BindExisting(Box<BindExistingRequest>),
    Status {
        installation: String,
    },
    Remove {
        installation: String,
        consent: Option<String>,
    },
    Inventory,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BindExistingRequest {
    pub installation: String,
    pub consent: String,
    pub cli: PathBuf,
    pub target: Target,
    pub spawn_fingerprint: String,
    pub description: String,
    pub upgrade: bool,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    DesktopProfile,
    StandaloneScript,
    Unknown,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedHostStatus {
    pub installation: String,
    pub origin: Origin,
    pub consent: Option<String>,
    pub phase: Phase,
    pub account: Option<String>,
    pub tunnel: Option<String>,
    pub failure: Option<Failure>,
    pub destination: Option<crate::relay::Destination>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagementReply {
    pub version: u16,
    pub hosts: Vec<ManagedHostStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn namespace() -> io::Result<PathBuf> {
    let runtime =
        festerm_sessiond::runtime_root().map_err(|_| refused("profile_host.runtime_namespace"))?;
    Ok(runtime
        .parent()
        .ok_or_else(|| refused("profile_host.runtime_namespace"))?
        .join("profile-hosts"))
}

pub fn installation_path(installation: &str) -> io::Result<PathBuf> {
    canonical_uuid(installation)?;
    Ok(namespace()?.join("installations").join(installation))
}

/// Foreground login takes an exclusive lease; desktop account-bound mutations
/// and token acquisition take a shared lease. Revocation/stop does not need it.
pub fn account_lease(exclusive: bool) -> io::Result<File> {
    let directory = setup_storage::private_directory(&namespace()?, false)?;
    let name = Path::new("account.lock");
    let file = match setup_storage::open_private_file(&directory, name, true) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            setup_storage::open_private_file(&directory, name, false)?
        }
        Err(error) => return Err(error),
    };
    if exclusive {
        file.try_lock_exclusive()?;
    } else {
        FileExt::try_lock_shared(&file)?;
    }
    Ok(file)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Prepared,
    Requested,
    Provisioning,
    Exposed,
    Revoking,
    CleanupPending,
    Revoked,
    CloudMissing,
}

impl Phase {
    pub const fn allows_hosting(self) -> bool {
        matches!(self, Self::Requested | Self::Provisioning | Self::Exposed)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Failure {
    SignInRequired,
    Unknown,
    NewShellApprovalRequired,
    CommandMismatch,
    HelperUnavailable,
    CreationUncertain,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub runtime_root: PathBuf,
    pub name: String,
    pub pid: u32,
    pub generation: u128,
    pub endpoint: String,
}

impl Target {
    fn validate(&self) -> io::Result<()> {
        let suffix = format!("{}-{}", self.pid, self.generation);
        if !self.runtime_root.is_absolute()
            || self.runtime_root.file_name() != Some(std::ffi::OsStr::new("sessiond"))
            || self.pid == 0
            || self.generation == 0
            || self.name.is_empty()
            || self.name.len() > 64
            || !self
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            || self.endpoint.len() > 4096
            || self.endpoint.chars().any(char::is_control)
            || !(self.endpoint.ends_with(&suffix)
                || self.endpoint.ends_with(&format!("{suffix}.sock")))
        {
            return Err(refused("profile_host.target_identity"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CloudReservation {
    pub account: String,
    pub name: String,
    pub ownership_label: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CloudIdentity {
    pub qualified_id: String,
    /// Opaque server creation/incarnation metadata, when the service supplies it.
    pub incarnation: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Consent {
    version: u16,
    pub installation: String,
    pub consent: String,
    pub spawn_fingerprint: String,
    pub phase: Phase,
    pub session_name: String,
    pub target: Option<Target>,
    pub cloud_reservation: Option<CloudReservation>,
    pub cloud: Option<CloudIdentity>,
    pub creation_pending: bool,
    #[serde(default)]
    pub creation_submitted: bool,
    pub worker_stopped: bool,
    pub failure: Option<Failure>,
}

impl Consent {
    fn validate(&self) -> io::Result<()> {
        canonical_uuid(&self.installation)?;
        canonical_uuid(&self.consent)?;
        if self.version != 1
            || self.spawn_fingerprint.is_empty()
            || self.spawn_fingerprint.len() > 256
            || !self.spawn_fingerprint.is_ascii()
            || self.spawn_fingerprint.chars().any(char::is_control)
            || self.session_name.is_empty()
            || self.session_name.len() > 64
            || !self
                .session_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            || self
                .target
                .as_ref()
                .is_some_and(|target| target.name != self.session_name)
            || (self.cloud.is_some() || self.creation_pending) && self.cloud_reservation.is_none()
            || self.cloud.is_some() && self.creation_pending
            || self.creation_submitted && !self.creation_pending
            || self.phase == Phase::Exposed && (self.target.is_none() || self.cloud.is_none())
            || self.phase == Phase::Revoked
                && (!self.worker_stopped || self.cloud.is_some() || self.creation_pending)
            || self.phase == Phase::CleanupPending && !self.worker_stopped
        {
            return Err(refused("profile_host.consent_identity"));
        }
        if let Some(target) = &self.target {
            target.validate()?;
        }
        if let Some(reservation) = &self.cloud_reservation {
            let id = uuid::Uuid::parse_str(&self.installation).expect("validated installation");
            let Some(label_id) = reservation.ownership_label.strip_prefix("festerm-owned-") else {
                return Err(refused("profile_host.cloud_identity"));
            };
            canonical_uuid(label_id)?;
            if reservation.account.is_empty()
                || reservation.account.len() > 256
                || !reservation
                    .account
                    .bytes()
                    .all(|byte| byte.is_ascii_digit())
                || reservation.name != format!("festerm-{}", id.simple())
            {
                return Err(refused("profile_host.cloud_identity"));
            }
            if let Some(cloud) = &self.cloud {
                let Some((name, cluster)) = cloud.qualified_id.rsplit_once('.') else {
                    return Err(refused("profile_host.cloud_identity"));
                };
                if name != reservation.name
                    || cluster.is_empty()
                    || cluster.len() > 32
                    || !cluster.bytes().all(|byte| byte.is_ascii_alphanumeric())
                    || cloud.incarnation.as_ref().is_some_and(|value| {
                        value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
                    })
                {
                    return Err(refused("profile_host.cloud_identity"));
                }
            }
        }
        Ok(())
    }
}

/// Every transaction holds only the short consent lock, never an installer,
/// subprocess or network operation. Disable can therefore fence a slow create.
pub struct Store {
    directory: File,
    installation: String,
}

impl Store {
    pub fn open(state: &Path, installation: &str, create: bool) -> io::Result<Self> {
        canonical_uuid(installation)?;
        Ok(Self {
            directory: setup_storage::private_directory(state, create)?,
            installation: installation.to_owned(),
        })
    }

    fn lock(&self) -> io::Result<File> {
        self.lock_until(std::time::Instant::now() + std::time::Duration::from_secs(2))
    }

    fn lock_until(&self, deadline: std::time::Instant) -> io::Result<File> {
        let file = match setup_storage::open_private_file(&self.directory, Path::new(LOCK), true) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                setup_storage::open_private_file(&self.directory, Path::new(LOCK), false)?
            }
            Err(error) => return Err(error),
        };
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => break,
                Err(error)
                    if festerm_sessiond::lock_is_contended(&error)
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => return Err(error),
            }
        }
        Ok(file)
    }

    fn load(&self) -> io::Result<Consent> {
        let bytes = setup_storage::read_private(&self.directory, Path::new(RECORD), 64 * 1024)?;
        let record: Consent =
            serde_json::from_slice(&bytes).map_err(|_| refused("profile_host.consent_decode"))?;
        record.validate()?;
        if record.installation != self.installation {
            return Err(refused("profile_host.installation_mismatch"));
        }
        Ok(record)
    }

    fn save(&self, record: &Consent) -> io::Result<()> {
        record.validate()?;
        let bytes =
            serde_json::to_vec(record).map_err(|_| refused("profile_host.consent_encode"))?;
        setup_storage::replace_private(&self.directory, Path::new(RECORD), &bytes)
    }

    pub fn read(&self) -> io::Result<Consent> {
        let _lock = self.lock()?;
        self.load()
    }

    /// Read for foreground projections without waiting on another transaction.
    pub fn try_read(&self) -> io::Result<Consent> {
        let _lock = self.lock_until(std::time::Instant::now())?;
        self.load()
    }

    pub fn prepare(
        &self,
        consent: &str,
        spawn_fingerprint: &str,
        session_name: &str,
    ) -> io::Result<()> {
        let _lock = self.lock()?;
        match self.load() {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(record)
                if record.phase == Phase::Prepared
                    && record.cloud.is_none()
                    && !record.creation_pending
                    && record.target.is_none()
                    || record.phase == Phase::Revoked
                        && record.worker_stopped
                        && record.cloud.is_none()
                        && !record.creation_pending => {}
            Ok(_) => return Err(refused("profile_host.existing_consent")),
            Err(error) => return Err(error),
        }
        self.save(&Consent {
            version: 1,
            installation: self.installation.clone(),
            consent: consent.to_owned(),
            spawn_fingerprint: spawn_fingerprint.to_owned(),
            session_name: session_name.to_owned(),
            phase: Phase::Prepared,
            target: None,
            cloud_reservation: None,
            cloud: None,
            creation_pending: false,
            creation_submitted: false,
            worker_stopped: true,
            failure: None,
        })
    }

    fn update(
        &self,
        consent: &str,
        change: impl FnOnce(&mut Consent) -> io::Result<()>,
    ) -> io::Result<Consent> {
        let _lock = self.lock()?;
        let mut record = self.load()?;
        if record.consent != consent {
            return Err(refused("profile_host.stale_consent"));
        }
        change(&mut record)?;
        self.save(&record)?;
        Ok(record)
    }

    /// Call only after the enabled configuration has been atomically saved.
    pub fn commit_requested(&self, consent: &str) -> io::Result<Consent> {
        self.update(consent, |record| {
            if record.phase != Phase::Prepared {
                return Err(refused("profile_host.not_prepared"));
            }
            record.phase = Phase::Requested;
            Ok(())
        })
    }

    pub fn cancel_prepared(&self, consent: &str) -> io::Result<Consent> {
        self.update(consent, |record| {
            if record.phase != Phase::Prepared
                || record.cloud.is_some()
                || record.creation_pending
                || record.target.is_some()
            {
                return Err(refused("profile_host.not_prepared"));
            }
            record.phase = Phase::Revoked;
            record.worker_stopped = true;
            Ok(())
        })
    }

    /// Separate user confirmation renews only the shell-launch request. Cloud
    /// identity and previously approved policies are not a new creation grant.
    pub fn prepare_new_shell(&self, consent: &str, fresh: &str) -> io::Result<Consent> {
        canonical_uuid(fresh)?;
        self.update(consent, |record| {
            require_active(record)?;
            if fresh == consent
                || record.failure != Some(Failure::NewShellApprovalRequired)
                || record.creation_pending
            {
                return Err(refused("profile_host.new_shell_not_authorized"));
            }
            record.consent = fresh.to_owned();
            record.phase = Phase::Prepared;
            record.target = None;
            record.failure = None;
            Ok(())
        })
    }

    pub fn bind_target(&self, consent: &str, target: Target) -> io::Result<Consent> {
        self.update(consent, |record| {
            require_active(record)?;
            if target.name != record.session_name {
                return Err(refused("profile_host.command_mismatch"));
            }
            if record.target.as_ref().is_some_and(|bound| bound != &target) {
                return Err(refused("profile_host.generation_changed"));
            }
            record.target = Some(target);
            if record.phase != Phase::Exposed {
                record.phase = Phase::Provisioning;
            }
            record.failure = None;
            Ok(())
        })
    }

    /// A pending reservation is reconciled, never submitted as a second create.
    pub fn reserve_creation(&self, consent: &str, account: &str) -> io::Result<Consent> {
        self.update(consent, |record| {
            require_active(record)?;
            if record.target.is_none()
                || record.cloud_reservation.is_some()
                || record.cloud.is_some()
                || record.creation_pending
            {
                return Err(refused("profile_host.creation_already_reserved"));
            }
            let id = uuid::Uuid::parse_str(&record.installation)
                .map_err(|_| refused("profile_host.consent_identity"))?;
            record.cloud_reservation = Some(CloudReservation {
                account: account.to_owned(),
                name: format!("festerm-{}", id.simple()),
                ownership_label: format!("festerm-owned-{}", uuid::Uuid::new_v4()),
            });
            record.creation_pending = true;
            record.creation_submitted = false;
            Ok(())
        })
    }

    /// The durable one-shot marker precedes spawning the create command. A
    /// retry may reconcile it, but cannot submit a second mutation.
    pub fn claim_creation_submission(&self, consent: &str) -> io::Result<bool> {
        let mut claimed = false;
        self.update(consent, |record| {
            require_active(record)?;
            if !record.creation_pending {
                return Err(refused("profile_host.creation_not_reserved"));
            }
            if !record.creation_submitted {
                record.creation_submitted = true;
                claimed = true;
            }
            Ok(())
        })?;
        Ok(claimed)
    }

    pub fn cancel_unsubmitted_creation(&self, consent: &str) -> io::Result<Consent> {
        self.update(consent, |record| {
            if record.phase != Phase::CleanupPending
                || !record.worker_stopped
                || !record.creation_pending
                || record.creation_submitted
            {
                return Err(refused("profile_host.creation_may_be_submitted"));
            }
            record.creation_pending = false;
            Ok(())
        })
    }

    /// Retain the result even after revocation, but never restore permission to
    /// host. The caller verifies the reservation's account and creation label.
    pub fn record_created(&self, consent: &str, cloud: CloudIdentity) -> io::Result<Consent> {
        self.update(consent, |record| {
            if !record.creation_pending {
                return Err(refused("profile_host.creation_not_reserved"));
            }
            record.cloud = Some(cloud);
            record.creation_pending = false;
            record.creation_submitted = false;
            if record.phase == Phase::Revoked {
                record.phase = Phase::CleanupPending;
            }
            Ok(())
        })
    }

    pub fn mark_exposed(&self, consent: &str) -> io::Result<Consent> {
        self.update(consent, |record| {
            require_active(record)?;
            if record.target.is_none() || record.cloud.is_none() {
                return Err(refused("profile_host.not_ready"));
            }
            record.phase = Phase::Exposed;
            record.worker_stopped = false;
            record.failure = None;
            Ok(())
        })
    }

    /// Keep this transaction short: it fences spawning/publication against
    /// revocation, not CLI/network operations or waiting for readiness.
    pub fn with_hosting_permit<T>(
        &self,
        consent: &str,
        target: &Target,
        qualified_id: &str,
        ownership_label: &str,
        spawn_fingerprint: &str,
        action: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        let _lock = self.lock()?;
        let record = self.load()?;
        require_active(&record)?;
        if record.consent != consent
            || record.spawn_fingerprint != spawn_fingerprint
            || record.target.as_ref() != Some(target)
            || record
                .cloud
                .as_ref()
                .map(|cloud| cloud.qualified_id.as_str())
                != Some(qualified_id)
            || record
                .cloud_reservation
                .as_ref()
                .map(|reservation| reservation.ownership_label.as_str())
                != Some(ownership_label)
        {
            return Err(refused("profile_host.stale_binding"));
        }
        action()
    }

    /// This durable fence precedes disabling/removing configuration or stopping
    /// the service, including when a subsequent configuration write fails.
    pub fn revoke(&self, consent: &str) -> io::Result<Consent> {
        self.update(consent, |record| {
            if !matches!(
                record.phase,
                Phase::Revoking | Phase::CleanupPending | Phase::Revoked
            ) {
                record.phase = Phase::Revoking;
            }
            record.failure = None;
            Ok(())
        })
    }

    /// Only call after exact owned service stop and worker-lock release.
    pub fn mark_stopped(&self, consent: &str) -> io::Result<Consent> {
        self.update(consent, |record| {
            if !matches!(
                record.phase,
                Phase::Revoking | Phase::CleanupPending | Phase::Revoked
            ) {
                return Err(refused("profile_host.not_revoking"));
            }
            record.worker_stopped = true;
            record.phase = if record.cloud.is_some() || record.creation_pending {
                Phase::CleanupPending
            } else {
                Phase::Revoked
            };
            Ok(())
        })
    }

    /// Call after verified exact-ID deletion or definitive authenticated absence.
    pub fn complete_deletion(&self, consent: &str) -> io::Result<Consent> {
        self.update(consent, |record| {
            if record.phase != Phase::CleanupPending
                || !record.worker_stopped
                || record.creation_pending
            {
                return Err(refused("profile_host.cleanup_not_ready"));
            }
            record.cloud = None;
            record.creation_pending = false;
            record.phase = Phase::Revoked;
            record.failure = None;
            Ok(())
        })
    }

    pub fn report_failure(&self, consent: &str, failure: Failure) -> io::Result<Consent> {
        self.update(consent, |record| {
            record.failure = Some(failure);
            Ok(())
        })
    }

    /// Only a definitive authenticated exact-ID not-found may use this path.
    /// A timeout, empty listing or fesTerm-initiated removal is not CloudMissing.
    pub fn mark_cloud_missing(&self, consent: &str) -> io::Result<Consent> {
        self.update(consent, |record| {
            require_active(record)?;
            if record.cloud.is_none() || record.creation_pending {
                return Err(refused("profile_host.cloud_identity"));
            }
            record.phase = Phase::CloudMissing;
            record.failure = None;
            Ok(())
        })
    }

    /// The caller obtains separate explicit recreation approval; the exact
    /// daemon binding is retained, so this cannot authorize another shell.
    pub fn request_recreation(&self, consent: &str) -> io::Result<Consent> {
        self.update(consent, |record| {
            if record.phase != Phase::CloudMissing
                || record.creation_pending
                || record.target.is_none()
            {
                return Err(refused("profile_host.recreation_not_authorized"));
            }
            record.phase = Phase::Requested;
            record.cloud = None;
            record.cloud_reservation = None;
            record.failure = None;
            Ok(())
        })
    }
}

fn require_active(record: &Consent) -> io::Result<()> {
    if record.phase.allows_hosting() {
        Ok(())
    } else {
        Err(refused("profile_host.consent_not_active"))
    }
}

fn canonical_uuid(value: &str) -> io::Result<()> {
    let id = uuid::Uuid::parse_str(value).map_err(|_| refused("profile_host.consent_identity"))?;
    if id.get_version_num() == 4 && id.to_string() == value {
        Ok(())
    } else {
        Err(refused("profile_host.consent_identity"))
    }
}

fn refused(code: &'static str) -> io::Error {
    io::Error::other(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> Target {
        Target {
            runtime_root: std::env::temp_dir().join("fesTerm").join("sessiond"),
            name: "configured-shell".into(),
            pid: 42,
            generation: 123,
            endpoint: "session-42-123".into(),
        }
    }

    #[test]
    fn foreground_consent_read_refuses_a_busy_transaction_without_waiting() {
        let state = crate::test_support::private_directory("festerm-profile-busy-read");
        let installation = uuid::Uuid::new_v4().to_string();
        let store = Store::open(&state, &installation, false).unwrap();
        let consent = uuid::Uuid::new_v4().to_string();
        store
            .prepare(&consent, "spawn-v1:test", "configured-shell")
            .unwrap();
        let lock = store.lock().unwrap();
        let started = std::time::Instant::now();
        let error = store.try_read().unwrap_err();
        assert!(festerm_sessiond::lock_is_contended(&error));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        drop(lock);
        assert_eq!(store.try_read().unwrap().consent, consent);
    }

    #[test]
    fn fresh_shell_approval_preserves_cloud_identity_and_pins_the_initial_name() {
        let state = crate::test_support::private_directory("festerm-profile-fresh-shell");
        let installation = uuid::Uuid::new_v4().to_string();
        let store = Store::open(&state, &installation, false).unwrap();
        let consent = uuid::Uuid::new_v4().to_string();
        let fresh = uuid::Uuid::new_v4().to_string();
        store
            .prepare(&consent, "spawn-v1:test", "configured-shell")
            .unwrap();
        store.commit_requested(&consent).unwrap();
        let mut wrong = target();
        wrong.name = "unapproved-shell".into();
        assert!(store.bind_target(&consent, wrong).is_err());
        assert!(store.read().unwrap().target.is_none());
        store.bind_target(&consent, target()).unwrap();
        let reservation = store.reserve_creation(&consent, "1234").unwrap();
        let cloud = CloudIdentity {
            qualified_id: format!("{}.usw3", reservation.cloud_reservation.unwrap().name),
            incarnation: None,
        };
        store.record_created(&consent, cloud.clone()).unwrap();
        store.mark_exposed(&consent).unwrap();
        assert!(store.prepare_new_shell(&consent, &fresh).is_err());
        store
            .report_failure(&consent, Failure::NewShellApprovalRequired)
            .unwrap();
        let prepared = store.prepare_new_shell(&consent, &fresh).unwrap();
        assert_eq!(prepared.phase, Phase::Prepared);
        assert_eq!(prepared.cloud, Some(cloud));
        assert!(prepared.target.is_none());
        assert!(store.commit_requested(&consent).is_err());
        assert!(store.bind_target(&fresh, target()).is_err());
        store.commit_requested(&fresh).unwrap();
        let mut replacement = target();
        replacement.generation += 1;
        replacement.endpoint = format!("session-{}-{}", replacement.pid, replacement.generation);
        store.bind_target(&fresh, replacement).unwrap();
        store.revoke(&fresh).unwrap();
        assert!(store
            .prepare_new_shell(&fresh, &uuid::Uuid::new_v4().to_string())
            .is_err());
        drop(store);
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn a_brief_concurrent_status_transaction_does_not_look_like_revocation() {
        let state = crate::test_support::private_directory("festerm-profile-contention");
        let installation = uuid::Uuid::new_v4().to_string();
        let store = Store::open(&state, &installation, false).unwrap();
        let consent = uuid::Uuid::new_v4().to_string();
        store
            .prepare(&consent, "spawn-v1:test", "configured-shell")
            .unwrap();
        let held = store.lock().unwrap();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            drop(held);
        });
        assert_eq!(store.read().unwrap().phase, Phase::Prepared);
        releaser.join().unwrap();
        drop(store);
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn cancelled_preparation_and_completed_revocation_need_fresh_explicit_consent() {
        let state = crate::test_support::private_directory("festerm-profile-renew");
        let installation = uuid::Uuid::new_v4().to_string();
        let store = Store::open(&state, &installation, false).unwrap();
        let old = uuid::Uuid::new_v4().to_string();
        let current = uuid::Uuid::new_v4().to_string();
        store
            .prepare(&old, "spawn-v1:test", "configured-shell")
            .unwrap();
        store.cancel_prepared(&old).unwrap();
        store
            .prepare(&current, "spawn-v1:test", "configured-shell")
            .unwrap();
        assert!(store.commit_requested(&old).is_err());
        store.commit_requested(&current).unwrap();
        assert!(store
            .prepare(&old, "spawn-v1:test", "configured-shell")
            .is_err());
        store.bind_target(&current, target()).unwrap();
        let reserved = store.reserve_creation(&current, "1234").unwrap();
        let cloud = CloudIdentity {
            qualified_id: format!("{}.usw3", reserved.cloud_reservation.unwrap().name),
            incarnation: None,
        };
        store.record_created(&current, cloud.clone()).unwrap();
        store.mark_exposed(&current).unwrap();
        assert_eq!(
            store.bind_target(&current, target()).unwrap().phase,
            Phase::Exposed
        );
        store.mark_cloud_missing(&current).unwrap();
        let recreated = store.request_recreation(&current).unwrap();
        assert_eq!(recreated.target, Some(target()));
        store.reserve_creation(&current, "1234").unwrap();
        store.record_created(&current, cloud).unwrap();
        store.revoke(&current).unwrap();
        store.mark_stopped(&current).unwrap();
        assert!(store
            .prepare(&old, "spawn-v1:test", "configured-shell")
            .is_err());
        assert!(store.request_recreation(&current).is_err());
        store.complete_deletion(&current).unwrap();
        store
            .prepare(&old, "spawn-v1:test", "configured-shell")
            .unwrap();
        assert!(store.commit_requested(&current).is_err());
        drop(store);
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn a_crash_before_submission_can_be_cancelled_but_an_uncertain_create_cannot() {
        let state = crate::test_support::private_directory("festerm-profile-submission");
        let installation = uuid::Uuid::new_v4().to_string();
        let store = Store::open(&state, &installation, false).unwrap();
        let consent = uuid::Uuid::new_v4().to_string();
        store
            .prepare(&consent, "spawn-v1:test", "configured-shell")
            .unwrap();
        store.commit_requested(&consent).unwrap();
        store.bind_target(&consent, target()).unwrap();
        store.reserve_creation(&consent, "1234").unwrap();
        store.revoke(&consent).unwrap();
        assert!(store.claim_creation_submission(&consent).is_err());
        store.mark_stopped(&consent).unwrap();
        store.cancel_unsubmitted_creation(&consent).unwrap();
        store.complete_deletion(&consent).unwrap();

        let next = uuid::Uuid::new_v4().to_string();
        store
            .prepare(&next, "spawn-v1:test", "configured-shell")
            .unwrap();
        store.commit_requested(&next).unwrap();
        store.bind_target(&next, target()).unwrap();
        store.reserve_creation(&next, "1234").unwrap();
        assert!(store.claim_creation_submission(&next).unwrap());
        drop(store);
        let store = Store::open(&state, &installation, false).unwrap();
        assert!(!store.claim_creation_submission(&next).unwrap());
        store.revoke(&next).unwrap();
        store.mark_stopped(&next).unwrap();
        assert!(store.cancel_unsubmitted_creation(&next).is_err());
        assert!(store.complete_deletion(&next).is_err());
        drop(store);
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn prepared_consent_cannot_host_and_a_different_or_revoked_consent_cannot_resume() {
        let state = crate::test_support::private_directory("festerm-profile-consent");
        let installation = uuid::Uuid::new_v4().to_string();
        let store = Store::open(&state, &installation, false).unwrap();
        let consent = uuid::Uuid::new_v4().to_string();
        store
            .prepare(&consent, "spawn-v1:test", "configured-shell")
            .unwrap();
        assert!(store.bind_target(&consent, target()).is_err());
        assert!(store
            .commit_requested(&uuid::Uuid::new_v4().to_string())
            .is_err());
        drop(store);

        let store = Store::open(&state, &installation, false).unwrap();
        assert_eq!(store.read().unwrap().phase, Phase::Prepared);
        store.commit_requested(&consent).unwrap();
        store.bind_target(&consent, target()).unwrap();
        let mut replacement = target();
        replacement.generation = 456;
        replacement.endpoint = "session-42-456".into();
        assert!(store.bind_target(&consent, replacement).is_err());
        store.revoke(&consent).unwrap();
        assert!(store.bind_target(&consent, target()).is_err());
        assert!(store.commit_requested(&consent).is_err());
        assert_eq!(store.mark_stopped(&consent).unwrap().phase, Phase::Revoked);
        drop(store);
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn late_cloud_creation_is_retained_for_cleanup_but_never_published_after_revocation() {
        let state = crate::test_support::private_directory("festerm-profile-create-race");
        let installation = uuid::Uuid::new_v4().to_string();
        let store = Store::open(&state, &installation, false).unwrap();
        let consent = uuid::Uuid::new_v4().to_string();
        store
            .prepare(&consent, "spawn-v1:test", "configured-shell")
            .unwrap();
        store.commit_requested(&consent).unwrap();
        store.bind_target(&consent, target()).unwrap();
        let reserved = store.reserve_creation(&consent, "1234").unwrap();
        assert!(store.reserve_creation(&consent, "1234").is_err());
        store.revoke(&consent).unwrap();
        assert!(store.complete_deletion(&consent).is_err());
        assert_eq!(
            store.mark_stopped(&consent).unwrap().phase,
            Phase::CleanupPending
        );
        assert!(store.complete_deletion(&consent).is_err());
        drop(store);

        let store = Store::open(&state, &installation, false).unwrap();
        let cloud = CloudIdentity {
            qualified_id: format!("{}.usw3", reserved.cloud_reservation.unwrap().name),
            incarnation: Some("server-incarnation".into()),
        };
        let late = store.record_created(&consent, cloud.clone()).unwrap();
        assert_eq!(late.phase, Phase::CleanupPending);
        assert_eq!(late.cloud, Some(cloud));
        assert!(store.mark_exposed(&consent).is_err());
        assert_eq!(
            store.complete_deletion(&consent).unwrap().phase,
            Phase::Revoked
        );
        assert!(store.reserve_creation(&consent, "1234").is_err());
        drop(store);
        std::fs::remove_dir_all(state).unwrap();
    }
}
