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

pub fn hosted_runtime() -> io::Result<PathBuf> {
    let product = if cfg!(windows) { "fesTerm" } else { "festerm" };
    let namespace = namespace()?;
    Ok(namespace
        .parent()
        .ok_or_else(|| refused("profile_host.runtime_namespace"))?
        .join("h")
        .join(product)
        .join("sessiond"))
}

pub fn previous_hosted_runtime() -> io::Result<PathBuf> {
    let product = if cfg!(windows) { "fesTerm" } else { "festerm" };
    Ok(namespace()?.join("runtime").join(product).join("sessiond"))
}

pub fn accepts_hosted_runtime(root: &Path) -> io::Result<bool> {
    Ok(root == hosted_runtime()? || root == previous_hosted_runtime()?)
}

pub fn accepts_runtime(root: &Path) -> io::Result<bool> {
    let legacy =
        festerm_sessiond::runtime_root().map_err(|_| refused("profile_host.runtime_namespace"))?;
    Ok(root == legacy || accepts_hosted_runtime(root)?)
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
    RestartRequired,
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
    fn validate_fields(&self) -> io::Result<()> {
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
        {
            return Err(refused("profile_host.target_identity"));
        }
        Ok(())
    }

    fn validate(&self) -> io::Result<()> {
        self.validate_fields()?;
        let suffix = format!("{}-{}", self.pid, self.generation);
        if !(self.endpoint.ends_with(&suffix) || self.endpoint.ends_with(&format!("{suffix}.sock")))
        {
            return Err(refused("profile_host.target_identity"));
        }
        Ok(())
    }

    fn validate_legacy(&self) -> io::Result<()> {
        self.validate_fields()?;
        if self.validate().is_ok() || self.endpoint == self.named_endpoint() {
            return Ok(());
        }
        Err(refused("profile_host.target_identity"))
    }

    #[cfg(unix)]
    fn named_endpoint(&self) -> String {
        self.runtime_root
            .join(format!("{}.sock", self.name))
            .to_string_lossy()
            .into_owned()
    }

    #[cfg(windows)]
    fn named_endpoint(&self) -> String {
        let name: String = self
            .name
            .chars()
            .map(|character| match character {
                ch if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') => ch,
                _ => '_',
            })
            .collect();
        format!(r"\\.\pipe\festerm-sessiond-{name}")
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
pub struct HostedLaunch {
    pub runtime_root: PathBuf,
    pub legacy: Option<Target>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosted_launch: Option<HostedLaunch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_account: Option<String>,
}

impl Consent {
    fn validate(&self) -> io::Result<()> {
        if self.selected_account.as_ref().is_some_and(|account| {
            account.is_empty()
                || account.len() > 256
                || !account.bytes().all(|byte| byte.is_ascii_digit())
                || self
                    .cloud_reservation
                    .as_ref()
                    .is_some_and(|reservation| &reservation.account != account)
        }) {
            return Err(refused("profile_host.account_identity"));
        }
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
        if let Some(launch) = &self.hosted_launch {
            let product = if cfg!(windows) { "fesTerm" } else { "festerm" };
            if !launch.runtime_root.is_absolute()
                || launch.runtime_root.file_name() != Some(std::ffi::OsStr::new("sessiond"))
                || launch.runtime_root.parent().and_then(Path::file_name)
                    != Some(std::ffi::OsStr::new(product))
                || self
                    .target
                    .as_ref()
                    .is_some_and(|target| target.runtime_root != launch.runtime_root)
            {
                return Err(refused("profile_host.runtime_namespace"));
            }
            if let Some(legacy) = &launch.legacy {
                legacy.validate_legacy()?;
                if legacy.name != self.session_name
                    || legacy.runtime_root == launch.runtime_root
                    || self.target.is_some()
                    || self.cloud.is_some()
                    || self.creation_pending
                {
                    return Err(refused("profile_host.restart_identity"));
                }
            }
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

pub struct HostedPreparation<'a> {
    pub installation: &'a str,
    pub consent: &'a str,
    pub spawn_fingerprint: &'a str,
    pub session_name: &'a str,
    pub launch: HostedLaunch,
    pub account: &'a str,
}

/// Publish only a complete Prepared record into the canonical inventory.
pub fn prepare_hosted_installation_in(
    namespace: &Path,
    request: HostedPreparation<'_>,
) -> io::Result<()> {
    canonical_uuid(request.installation)?;
    let staging = namespace.join("preparing");
    let installations = namespace.join("installations");
    let staging_parent = setup_storage::private_directory(&staging, true)?;
    let destination_parent = setup_storage::private_directory(&installations, false)?;
    let name = Path::new(request.installation);
    #[cfg(unix)]
    let created = festerm_unix_security::create_private_directory(&staging_parent, name)?;
    #[cfg(windows)]
    let created =
        festerm_windows_security::create_current_user_only_directory(&staging_parent, name)?;
    drop(created);

    let staged_path = staging.join(name);
    let store = Store::open(&staged_path, request.installation, false)?;
    store.prepare_hosted(
        request.consent,
        request.spawn_fingerprint,
        request.session_name,
        request.launch,
    )?;
    store.select_account(request.consent, request.account)?;
    drop(store);

    #[cfg(unix)]
    {
        festerm_unix_security::rename_noreplace(&staging_parent, name, &destination_parent, name)?;
        staging_parent.sync_all()?;
        destination_parent.sync_all()?;
    }
    #[cfg(windows)]
    {
        let directory = setup_storage::private_directory(&staged_path, false)?;
        let mover =
            festerm_windows_security::open_private_directory_for_move(&staging_parent, name)?;
        if !festerm_windows_security::same_file_identity(&directory, &mover)? {
            return Err(refused("profile_host.prepared_state_changed"));
        }
        festerm_windows_security::rename_private_directory_noreplace(
            &mover,
            &destination_parent,
            name,
        )?;
    }
    Ok(())
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
            hosted_launch: None,
            selected_account: None,
        })
    }

    pub fn prepare_hosted(
        &self,
        consent: &str,
        spawn_fingerprint: &str,
        session_name: &str,
        launch: HostedLaunch,
    ) -> io::Result<()> {
        self.prepare(consent, spawn_fingerprint, session_name)?;
        self.update(consent, |record| {
            if record.phase != Phase::Prepared {
                return Err(refused("profile_host.not_prepared"));
            }
            if launch.legacy.is_some() {
                record.failure = Some(Failure::RestartRequired);
            }
            record.hosted_launch = Some(launch);
            Ok(())
        })?;
        Ok(())
    }

    pub fn select_account(&self, consent: &str, account: &str) -> io::Result<()> {
        self.update(consent, |record| {
            if !matches!(record.phase, Phase::Prepared | Phase::Requested)
                || record.selected_account.is_some()
                || record.cloud_reservation.is_some()
            {
                return Err(refused("profile_host.not_prepared"));
            }
            record.selected_account = Some(account.to_owned());
            Ok(())
        })?;
        Ok(())
    }

    /// Only an explicit restart/open may consume a deferred launch request.
    pub fn authorize_hosted_start(&self, consent: &str, expected: &Target) -> io::Result<Consent> {
        self.update(consent, |record| {
            if !record.phase.allows_hosting() || record.target.is_some() {
                return Err(refused("profile_host.consent_not_active"));
            }
            let launch = record
                .hosted_launch
                .as_mut()
                .ok_or_else(|| refused("profile_host.restart_identity"))?;
            if launch.legacy.as_ref() != Some(expected) {
                return Err(refused("profile_host.restart_identity"));
            }
            require_generation_ended(expected)?;
            Ok(())
        })
    }

    pub fn bind_restarted_target(
        &self,
        consent: &str,
        expected: &Target,
        target: Target,
    ) -> io::Result<Consent> {
        self.update(consent, |record| {
            if !record.phase.allows_hosting() || record.target.is_some() {
                return Err(refused("profile_host.consent_not_active"));
            }
            let launch = record
                .hosted_launch
                .as_mut()
                .ok_or_else(|| refused("profile_host.restart_identity"))?;
            if launch.legacy.as_ref() != Some(expected) || target.name != record.session_name {
                return Err(refused("profile_host.restart_identity"));
            }
            require_generation_ended(expected)?;
            launch.legacy = None;
            record.target = Some(target);
            record.phase = Phase::Provisioning;
            record.failure = None;
            Ok(())
        })
    }

    pub fn defer_hosted_start(&self, consent: &str, legacy: Target) -> io::Result<Consent> {
        self.update(consent, |record| {
            require_active(record)?;
            if record.target.is_some() || record.cloud.is_some() || record.creation_pending {
                return Err(refused("profile_host.restart_identity"));
            }
            let launch = record
                .hosted_launch
                .as_mut()
                .ok_or_else(|| refused("profile_host.restart_identity"))?;
            if launch.legacy.is_some() {
                return Err(refused("profile_host.restart_identity"));
            }
            launch.legacy = Some(legacy);
            record.failure = Some(Failure::RestartRequired);
            Ok(())
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
            if !record.phase.allows_hosting() {
                return Err(refused("profile_host.consent_not_active"));
            }
            if fresh == consent
                || record.failure != Some(Failure::NewShellApprovalRequired)
                || record.creation_pending
            {
                return Err(refused("profile_host.new_shell_not_authorized"));
            }
            if let Some(target) = &record.target {
                require_generation_ended(target)?;
            }
            if let Some(legacy) = record
                .hosted_launch
                .as_ref()
                .and_then(|launch| launch.legacy.as_ref())
            {
                require_generation_ended(legacy)?;
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

fn require_generation_ended(expected: &Target) -> io::Result<()> {
    if festerm_sessiond::process_alive(expected.pid)
        || festerm_sessiond::list_running_sessions_strict_in(&expected.runtime_root)
            .map_err(|_| refused("profile_host.restart_identity"))?
            .iter()
            .any(|running| running.session.name == expected.name)
    {
        return Err(refused("profile_host.restart_required"));
    }
    Ok(())
}

fn require_active(record: &Consent) -> io::Result<()> {
    if record
        .hosted_launch
        .as_ref()
        .is_some_and(|launch| launch.legacy.is_some())
    {
        Err(refused("profile_host.restart_required"))
    } else if record.phase.allows_hosting() {
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
    fn hosted_preparation_publishes_only_complete_records_without_overwriting() {
        let root = crate::test_support::private_directory("festerm-profile-publish");
        setup_storage::private_directory(&root.join("installations"), true).unwrap();
        let product = if cfg!(windows) { "fesTerm" } else { "festerm" };
        let runtime = root.join("runtime").join(product).join("sessiond");
        let installation = uuid::Uuid::new_v4().to_string();
        let consent = uuid::Uuid::new_v4().to_string();
        let prepare = |id: &str, consent: &str, account: &str| {
            prepare_hosted_installation_in(
                &root,
                HostedPreparation {
                    installation: id,
                    consent,
                    spawn_fingerprint: "spawn-v1:test",
                    session_name: "configured-shell",
                    launch: HostedLaunch {
                        runtime_root: runtime.clone(),
                        legacy: None,
                    },
                    account,
                },
            )
        };
        let failed = uuid::Uuid::new_v4().to_string();
        assert!(prepare(&failed, &consent, "not-an-account").is_err());
        assert!(!root.join("installations").join(&failed).exists());
        assert!(root.join("preparing").join(&failed).exists());

        prepare(&installation, &consent, "1234").unwrap();
        let path = root.join("installations").join(&installation);
        let store = Store::open(&path, &installation, false).unwrap();
        let saved = store.read().unwrap();
        assert_eq!(saved.phase, Phase::Prepared);
        assert_eq!(saved.selected_account.as_deref(), Some("1234"));
        assert_eq!(saved.hosted_launch.unwrap().runtime_root, runtime);
        drop(store);

        let other = uuid::Uuid::new_v4().to_string();
        assert!(prepare(&installation, &other, "5678").is_err());
        let store = Store::open(&path, &installation, false).unwrap();
        assert_eq!(store.read().unwrap().consent, consent);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selected_account_fences_initial_reservation_before_an_installation_exists() {
        let state = crate::test_support::private_directory("festerm-profile-account");
        let installation = uuid::Uuid::new_v4().to_string();
        let store = Store::open(&state, &installation, false).unwrap();
        let consent = uuid::Uuid::new_v4().to_string();
        store
            .prepare(&consent, "spawn-v1:test", "configured-shell")
            .unwrap();
        assert!(store.select_account(&consent, "not-an-account").is_err());
        store.select_account(&consent, "1234").unwrap();
        store.commit_requested(&consent).unwrap();
        store.bind_target(&consent, target()).unwrap();
        assert!(store.reserve_creation(&consent, "5678").is_err());
        assert!(store.read().unwrap().cloud_reservation.is_none());
        assert_eq!(
            store.read().unwrap().selected_account.as_deref(),
            Some("1234")
        );
        assert_eq!(
            store
                .reserve_creation(&consent, "1234")
                .unwrap()
                .cloud_reservation
                .unwrap()
                .account,
            "1234"
        );
        assert!(store.select_account(&consent, "5678").is_err());
        drop(store);
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn deferred_restart_survives_reload_and_cannot_bind_publish_or_override_identity() {
        let state = crate::test_support::private_directory("festerm-profile-deferred-restart");
        let installation = uuid::Uuid::new_v4().to_string();
        let store = Store::open(&state, &installation, false).unwrap();
        let consent = uuid::Uuid::new_v4().to_string();
        let mut legacy = target();
        legacy.pid = std::process::id();
        legacy.endpoint = format!("session-{}-{}", legacy.pid, legacy.generation);
        let product = if cfg!(windows) { "fesTerm" } else { "festerm" };
        let root = state.join("runtime").join(product).join("sessiond");
        store
            .prepare_hosted(
                &consent,
                "spawn-v1:test",
                &legacy.name,
                HostedLaunch {
                    runtime_root: root.clone(),
                    legacy: Some(legacy.clone()),
                },
            )
            .unwrap();
        store.commit_requested(&consent).unwrap();
        drop(store);
        let store = Store::open(&state, &installation, false).unwrap();
        assert_eq!(
            store.read().unwrap().hosted_launch.unwrap().legacy,
            Some(legacy.clone())
        );
        let mut hosted = legacy.clone();
        hosted.runtime_root = root;
        assert!(store.bind_target(&consent, hosted).is_err());
        assert!(store.reserve_creation(&consent, "1234").is_err());
        assert!(store.mark_exposed(&consent).is_err());
        assert!(store.authorize_hosted_start(&consent, &legacy).is_err());
        let mut replacement = legacy.clone();
        replacement.generation += 1;
        replacement.endpoint = format!("session-{}-{}", replacement.pid, replacement.generation);
        assert!(store
            .authorize_hosted_start(&consent, &replacement)
            .is_err());
        store.revoke(&consent).unwrap();
        assert!(store.authorize_hosted_start(&consent, &legacy).is_err());
        drop(store);
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn named_legacy_endpoint_is_deferred_without_authorizing_live_replacement() {
        let state = crate::test_support::private_directory("festerm-profile-named-legacy");
        let installation = uuid::Uuid::new_v4().to_string();
        let consent = uuid::Uuid::new_v4().to_string();
        let store = Store::open(&state, &installation, false).unwrap();
        let mut legacy = target();
        legacy.runtime_root = state.join("legacy").join("sessiond");
        legacy.pid = std::process::id();
        legacy.endpoint = legacy.named_endpoint();
        let product = if cfg!(windows) { "fesTerm" } else { "festerm" };
        let hosted = state.join("runtime").join(product).join("sessiond");
        store
            .prepare_hosted(
                &consent,
                "spawn-v1:test",
                &legacy.name,
                HostedLaunch {
                    runtime_root: hosted,
                    legacy: Some(legacy.clone()),
                },
            )
            .unwrap();
        store.commit_requested(&consent).unwrap();
        assert!(store.authorize_hosted_start(&consent, &legacy).is_err());
        assert_eq!(
            store.read().unwrap().hosted_launch.unwrap().legacy,
            Some(legacy)
        );
        drop(store);
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn interrupted_authorized_restart_keeps_the_gate_until_exact_binding() {
        let state = crate::test_support::private_directory("festerm-profile-interrupted-restart");
        let installation = uuid::Uuid::new_v4().to_string();
        let store = Store::open(&state, &installation, false).unwrap();
        let consent = uuid::Uuid::new_v4().to_string();
        let mut legacy = target();
        legacy.pid = i32::MAX as u32;
        legacy.endpoint = format!("session-{}-{}", legacy.pid, legacy.generation);
        legacy.runtime_root = state.join("legacy").join("sessiond");
        assert!(!festerm_sessiond::process_alive(legacy.pid));
        let product = if cfg!(windows) { "fesTerm" } else { "festerm" };
        let root = state.join("runtime").join(product).join("sessiond");
        store
            .prepare_hosted(
                &consent,
                "spawn-v1:test",
                &legacy.name,
                HostedLaunch {
                    runtime_root: root.clone(),
                    legacy: Some(legacy.clone()),
                },
            )
            .unwrap();
        store.commit_requested(&consent).unwrap();
        store.authorize_hosted_start(&consent, &legacy).unwrap();
        drop(store);
        let store = Store::open(&state, &installation, false).unwrap();
        assert_eq!(
            store.read().unwrap().hosted_launch.unwrap().legacy,
            Some(legacy.clone())
        );
        assert!(store.bind_target(&consent, target()).is_err());
        assert!(store.reserve_creation(&consent, "1234").is_err());
        store
            .report_failure(&consent, Failure::NewShellApprovalRequired)
            .unwrap();
        let fresh = uuid::Uuid::new_v4().to_string();
        let prepared = store.prepare_new_shell(&consent, &fresh).unwrap();
        assert_eq!(prepared.hosted_launch.unwrap().legacy, Some(legacy.clone()));
        store.commit_requested(&fresh).unwrap();
        assert!(store.reserve_creation(&fresh, "1234").is_err());
        assert!(store
            .bind_restarted_target(&fresh, &legacy, target())
            .is_err());
        let mut hosted = target();
        hosted.runtime_root = root;
        let bound = store
            .bind_restarted_target(&fresh, &legacy, hosted.clone())
            .unwrap();
        assert_eq!(bound.target, Some(hosted));
        assert!(bound.hosted_launch.unwrap().legacy.is_none());
        assert!(store.reserve_creation(&fresh, "1234").is_ok());
        drop(store);
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn hosted_binding_is_pinned_to_the_prepared_runtime() {
        let state = crate::test_support::private_directory("festerm-profile-root-binding");
        let installation = uuid::Uuid::new_v4().to_string();
        let store = Store::open(&state, &installation, false).unwrap();
        let consent = uuid::Uuid::new_v4().to_string();
        let product = if cfg!(windows) { "fesTerm" } else { "festerm" };
        let root = state.join("runtime").join(product).join("sessiond");
        store
            .prepare_hosted(
                &consent,
                "spawn-v1:test",
                "configured-shell",
                HostedLaunch {
                    runtime_root: root.clone(),
                    legacy: None,
                },
            )
            .unwrap();
        store.commit_requested(&consent).unwrap();
        assert!(store.bind_target(&consent, target()).is_err());
        assert!(store.read().unwrap().target.is_none());
        let mut hosted = target();
        hosted.runtime_root = root;
        assert_eq!(
            store.bind_target(&consent, hosted.clone()).unwrap().target,
            Some(hosted)
        );
        drop(store);
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn hosted_runtime_accepts_short_and_previous_namespaces_only() {
        let legacy = festerm_sessiond::runtime_root().unwrap();
        let current = hosted_runtime().unwrap();
        let previous = previous_hosted_runtime().unwrap();
        let product = if cfg!(windows) { "fesTerm" } else { "festerm" };
        assert_eq!(
            current,
            legacy
                .parent()
                .unwrap()
                .join("h")
                .join(product)
                .join("sessiond")
        );
        assert_eq!(
            previous,
            namespace()
                .unwrap()
                .join("runtime")
                .join(product)
                .join("sessiond")
        );
        assert_ne!(current, previous);
        assert!(accepts_runtime(&legacy).unwrap());
        assert!(accepts_hosted_runtime(&current).unwrap());
        assert!(accepts_hosted_runtime(&previous).unwrap());
        assert!(!accepts_hosted_runtime(&legacy).unwrap());
        assert!(!accepts_runtime(&legacy.with_file_name("other")).unwrap());
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
        drop(store);
        std::fs::remove_dir_all(state).unwrap();
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
        let mut retired = target();
        retired.pid = i32::MAX as u32;
        retired.endpoint = format!("session-{}-{}", retired.pid, retired.generation);
        retired.runtime_root = state.join("legacy").join("sessiond");
        assert!(!festerm_sessiond::process_alive(retired.pid));
        store.bind_target(&consent, retired).unwrap();
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
    fn fresh_shell_approval_refuses_a_live_recorded_generation() {
        let state = crate::test_support::private_directory("festerm-profile-live-shell");
        let installation = uuid::Uuid::new_v4().to_string();
        let consent = uuid::Uuid::new_v4().to_string();
        let store = Store::open(&state, &installation, false).unwrap();
        store
            .prepare(&consent, "spawn-v1:test", "configured-shell")
            .unwrap();
        store.commit_requested(&consent).unwrap();
        let mut live = target();
        live.pid = std::process::id();
        live.endpoint = format!("session-{}-{}", live.pid, live.generation);
        store.bind_target(&consent, live.clone()).unwrap();
        store
            .report_failure(&consent, Failure::NewShellApprovalRequired)
            .unwrap();
        assert!(store
            .prepare_new_shell(&consent, &uuid::Uuid::new_v4().to_string())
            .is_err());
        assert_eq!(store.read().unwrap().target, Some(live));
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
