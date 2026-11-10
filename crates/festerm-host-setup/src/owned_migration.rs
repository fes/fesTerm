//! Explicit protected legacy inventory and crash-resumable identity reservation.
//! Neither reservation nor imported metadata is daemon, owner or cloud authority.

use crate::{
    lifecycle::{Phase as HostPhase, Snapshot, Store},
    protected_state::{self, Lease},
};
use festerm_sessiond::{private_storage as storage, NativeSessionIdentity};
use festerm_tunnel_bridge::{
    host_identity::HostIdentity,
    profile_hosting::{self, Consent},
    setup_storage,
};
use fs2::FileExt;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs::File, io, path::Path};
use uuid::Uuid;

const INTENT: &str = "host-migration-intent.json";
const JOURNAL: &str = "host-migration-journal.json";
const CLAIM: &str = "host-migration-claim.json";
const HOST_IDENTITY: &str = "host-identity";
const LIMIT: usize = 4 * 1024 * 1024;
const SOURCE_LIMIT: usize = 64 * 1024;
const MAX_SOURCES: usize = 8;
#[cfg(feature = "owner-enrollment")]
mod binding;
#[cfg(feature = "owner-enrollment")]
pub use binding::{approve_migration_owner, binding_status, has_bindings, BindingJournal};
#[cfg(feature = "owner-enrollment")]
pub(crate) use binding::{check_migration_owner, check_owner_initialization};

fn refused(code: &'static str) -> io::Error {
    io::Error::other(code)
}

fn uuid(value: &str) -> io::Result<()> {
    if Uuid::parse_str(value).is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == value) {
        Ok(())
    } else {
        Err(refused("host.migration_invalid_identity"))
    }
}

fn encode(value: &impl Serialize) -> io::Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value).map_err(|_| refused("host.migration_invalid_record"))?;
    if bytes.len() > LIMIT {
        return Err(refused("host.migration_record_too_large"));
    }
    Ok(bytes)
}

fn digest(value: &impl Serialize) -> io::Result<String> {
    Ok(Sha256::digest(encode(value)?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn read<T: DeserializeOwned>(directory: &File, name: &str, limit: usize) -> io::Result<T> {
    serde_json::from_slice(&storage::read_private(directory, Path::new(name), limit)?)
        .map_err(|_| refused("host.migration_invalid_record"))
}

fn optional<T: DeserializeOwned>(directory: &File, name: &str) -> io::Result<Option<T>> {
    match read(directory, name, LIMIT) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn raw(directory: &File, name: &str) -> io::Result<String> {
    let bytes = storage::read_private(directory, Path::new(name), SOURCE_LIMIT)?;
    std::str::from_utf8(&bytes)
        .map(str::to_owned)
        .map_err(|_| refused("host.migration_invalid_source_receipt"))
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Source {
    DesktopProfile { installation: String },
    StandaloneScript { installation: String },
}

impl Source {
    fn installation(&self) -> &str {
        match self {
            Self::DesktopProfile { installation } | Self::StandaloneScript { installation } => {
                installation
            }
        }
    }
}

/// Caller-supplied mapping only. Names, logical IDs and selection files grant nothing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub source: Source,
    pub logical_id: String,
    #[serde(default)]
    pub label: String,
}

pub fn read_selections(path: &Path) -> io::Result<Vec<Selection>> {
    if !path.is_absolute() {
        return Err(refused("host.migration_absolute_selection_path_required"));
    }
    let directory = storage::private_directory(
        path.parent()
            .ok_or_else(|| refused("host.migration_invalid_selection_path"))?,
        false,
    )?;
    let values: Vec<Selection> = read(
        &directory,
        path.file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| refused("host.migration_invalid_selection_path"))?,
        SOURCE_LIMIT,
    )?;
    selections(&values)?;
    Ok(values)
}

fn selections(values: &[Selection]) -> io::Result<()> {
    if values.is_empty() || values.len() > MAX_SOURCES {
        return Err(refused("host.migration_select_one_to_eight_sources"));
    }
    let mut sources = BTreeSet::new();
    let mut logical = BTreeSet::new();
    for selection in values {
        uuid(selection.source.installation())?;
        uuid(&selection.logical_id)?;
        if selection.label.len() > 256
            || selection.label.chars().any(char::is_control)
            || !sources.insert(&selection.source)
            || !logical.insert(&selection.logical_id)
        {
            return Err(refused("host.migration_duplicate_or_invalid_mapping"));
        }
    }
    Ok(())
}

/// Native generation evidence alone is not binding capability. Display-only
/// negotiation never replaces the immutable protected reservation or fresh admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Availability {
    AuthenticatedBindingUnavailable,
    AuthenticatedBindingAvailable,
    AlreadyBound,
    NativeGenerationUnavailable,
    CleanupPending,
    RevokedOrIncomplete,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryRow {
    pub selection: Selection,
    pub account: String,
    pub original_host_id: String,
    pub original_key_fingerprint: String,
    pub original_cloud_id: Option<String>,
    pub original_cloud_incarnation: Option<String>,
    pub original_consent: Option<String>,
    pub target: Option<profile_hosting::Target>,
    pub availability: Availability,
    /// There is deliberately no live stop operation or inherited confirmation.
    pub live_stop_authorized: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    pub preview_id: String,
    pub destination: Snapshot,
    pub account: String,
    pub mappings: Vec<InventoryRow>,
    pub daemon_binding_supported: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetainedSource {
    row: InventoryRow,
    installation_receipt: String,
    ownership_receipt: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    destination: Snapshot,
    account: String,
    sources: Vec<RetainedSource>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u16,
    operation: String,
    preview_id: String,
    plan: Plan,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Reserving,
    AwaitingNativeBinding,
    Deferred,
    Bound,
    Switched,
    RollbackPending,
    RolledBack,
}

/// Protected history, not current liveness, endpoint readiness or owner consent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    version: u16,
    pub operation: String,
    pub preview_id: String,
    pub destination_installation: String,
    pub destination_host_id: Option<String>,
    pub destination_key_fingerprint: Option<String>,
    pub account: String,
    pub phase: Phase,
    pub claimed_sources: BTreeSet<Source>,
    pub released_sources: BTreeSet<Source>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    version: u16,
    source: Source,
    source_host_id: String,
    source_key_fingerprint: String,
    source_receipt_digest: String,
    destination_installation: String,
    destination_host_id: String,
    destination_key_fingerprint: String,
    operation: String,
    released: bool,
}

// These fields retain the legacy receipt verbatim. Optional file/observation
// policies are deliberately opaque metadata: migration cannot import permissions.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyInstallation {
    version: u16,
    #[serde(default)]
    origin: Option<DesktopOrigin>,
    id: String,
    executable: std::path::PathBuf,
    cli: std::path::PathBuf,
    runtime_root: std::path::PathBuf,
    account: String,
    tunnel: Option<String>,
    phase: String,
    target: Option<LegacyTarget>,
    fingerprint: String,
    #[serde(default)]
    no_takeover: bool,
    #[serde(default)]
    filesystem: Option<serde_json::Value>,
    #[serde(default)]
    observation: Option<serde_json::Value>,
    #[serde(default)]
    recovery: Option<serde_json::Value>,
    #[serde(default)]
    upgrading_from: Vec<std::path::PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyTarget {
    name: String,
    pid: u32,
    generation: u128,
    endpoint: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DesktopOrigin {
    kind: String,
    consent: String,
    spawn_fingerprint: String,
    ownership_label: String,
    incarnation: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScriptOwnership {
    version: u16,
    installation: String,
    fingerprint: String,
    label: String,
    qualified: Option<String>,
    incarnation: Option<String>,
}

struct Loaded {
    directory: File,
    profile: Option<profile_hosting::Store>,
    consent: Option<Consent>,
    retained: RetainedSource,
    probe: NativeProbe,
    _leases: Vec<Lease>,
}

/// Canonical same-user namespaces only. There is no account-wide discovery,
/// public import path or caller-supplied runtime/identity filename.
pub struct Inventory {
    desktop: std::path::PathBuf,
    script: std::path::PathBuf,
    legacy_runtime: std::path::PathBuf,
    #[cfg(test)]
    native_evidence: Option<NativeProbe>,
}

type NativeProbe = fn(&profile_hosting::Target, Option<&String>) -> io::Result<bool>;

impl Inventory {
    #[cfg(feature = "owner-enrollment")]
    fn negotiate_preview(&self, rows: &mut [InventoryRow]) -> io::Result<bool> {
        #[cfg(test)]
        if self.native_evidence.is_some() {
            return Ok(false);
        }
        binding::preview_native(rows)
    }
    pub fn current_user() -> io::Result<Self> {
        Ok(Self {
            desktop: profile_hosting::namespace()?,
            script: setup_storage::script_namespace()?,
            legacy_runtime: festerm_sessiond::runtime_root()
                .map_err(|_| refused("host.migration_runtime_namespace_unavailable"))?,
            #[cfg(test)]
            native_evidence: None,
        })
    }

    fn probe(&self) -> NativeProbe {
        #[cfg(test)]
        if let Some(probe) = self.native_evidence {
            return probe;
        }
        exact_native
    }

    fn accepts_runtime(&self, root: &Path) -> bool {
        let product = if cfg!(windows) { "fesTerm" } else { "festerm" };
        root == self.legacy_runtime
            || self
                .desktop
                .parent()
                .is_some_and(|parent| root == parent.join("h").join(product).join("sessiond"))
            || root == self.desktop.join("runtime").join(product).join("sessiond")
    }

    fn directory(&self, source: &Source) -> io::Result<(File, Vec<Lease>)> {
        uuid(source.installation())?;
        let mut leases = Vec::new();
        let state = match source {
            Source::DesktopProfile { installation } => {
                self.desktop.join("installations").join(installation)
            }
            Source::StandaloneScript { .. } => {
                let root = storage::private_directory(&self.script, false)?;
                leases.push(Lease::exclusive(storage::open_private_file(
                    &root,
                    Path::new("installer.lock"),
                    false,
                )?)?);
                self.script.join("state")
            }
        };
        let directory = storage::private_directory(&state, false)?;
        leases.push(Lease::exclusive(storage::open_private_file(
            &directory,
            Path::new("installer.lock"),
            false,
        )?)?);
        Ok((directory, leases))
    }

    fn load(&self, selection: &Selection, account: &str) -> io::Result<Loaded> {
        let (directory, leases) = self.directory(&selection.source)?;
        let installation_receipt = raw(&directory, "installation.json")?;
        let record: LegacyInstallation = serde_json::from_str(&installation_receipt)
            .map_err(|_| refused("host.migration_invalid_source_receipt"))?;
        let identity =
            HostIdentity::load_from_private_directory(&directory, Path::new("host-identity.key"))?;
        if record.id != selection.source.installation()
            || record.fingerprint != identity.fingerprint()?
            || record.account != account
            || account.is_empty()
            || account.len() > 256
            || !account.bytes().all(|byte| byte.is_ascii_digit())
            || !record.cli.is_absolute()
            || !record.executable.is_absolute()
            || !record.runtime_root.is_absolute()
            || record.runtime_root.file_name() != Some(std::ffi::OsStr::new("sessiond"))
            || record.upgrading_from.len() > 8
            || record.upgrading_from.iter().any(|path| !path.is_absolute())
        {
            return Err(refused(
                "host.migration_source_account_key_or_identity_mismatch",
            ));
        }
        let target = record
            .target
            .as_ref()
            .map(|target| profile_hosting::Target {
                runtime_root: record.runtime_root.clone(),
                name: target.name.clone(),
                pid: target.pid,
                generation: target.generation,
                endpoint: target.endpoint.clone(),
            });
        if target.as_ref().is_some_and(|target| {
            target.pid == 0
                || target.generation == 0
                || target.name.is_empty()
                || target.name.len() > 64
                || !target
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
                || target.endpoint.len() > 4096
                || target.endpoint.chars().any(char::is_control)
        }) {
            return Err(refused("host.migration_invalid_native_identity"));
        }
        let (profile, consent, ownership_receipt, incarnation, active, cleanup) =
            match (&selection.source, &record.origin) {
                (Source::DesktopProfile { installation }, Some(origin))
                    if record.version == 2 && origin.kind == "desktop-profile" =>
                {
                    if !self.accepts_runtime(&record.runtime_root) {
                        return Err(refused("host.migration_foreign_runtime_namespace"));
                    }
                    let state = self.desktop.join("installations").join(installation);
                    // A missing lock is lost protected evidence, not a new consent.
                    storage::open_private_file(
                        &directory,
                        Path::new("profile-consent.lock"),
                        false,
                    )?;
                    let profile = profile_hosting::Store::open(&state, installation, false)?;
                    let grant = profile.try_read()?;
                    let receipt = raw(&directory, "profile-consent.json")?;
                    let retained_grant: Consent = serde_json::from_str(&receipt)
                        .map_err(|_| refused("host.migration_invalid_source_receipt"))?;
                    let reservation = grant
                        .cloud_reservation
                        .as_ref()
                        .ok_or_else(|| refused("host.migration_source_ownership_missing"))?;
                    if retained_grant != grant
                        || origin.consent != grant.consent
                        || origin.spawn_fingerprint != grant.spawn_fingerprint
                        || origin.ownership_label != reservation.ownership_label
                        || reservation.account != account
                        || grant.target != target
                        || grant.cloud.as_ref().map(|cloud| &cloud.qualified_id)
                            != record.tunnel.as_ref()
                        || grant
                            .cloud
                            .as_ref()
                            .and_then(|cloud| cloud.incarnation.as_ref())
                            != origin.incarnation.as_ref()
                        || record.recovery.is_some()
                    {
                        return Err(refused("host.migration_source_receipt_mismatch"));
                    }
                    let active = grant.phase == profile_hosting::Phase::Exposed;
                    let cleanup = matches!(
                        grant.phase,
                        profile_hosting::Phase::Revoking | profile_hosting::Phase::CleanupPending
                    );
                    (
                        Some(profile),
                        Some(grant),
                        receipt,
                        origin.incarnation.clone(),
                        active,
                        cleanup,
                    )
                }
                (Source::StandaloneScript { .. }, None) if record.version == 1 => {
                    let receipt = raw(&directory, "script-ownership.json")?;
                    let ownership: ScriptOwnership = serde_json::from_str(&receipt)
                        .map_err(|_| refused("host.migration_invalid_source_receipt"))?;
                    let label = ownership
                        .label
                        .strip_prefix("festerm-owned-")
                        .ok_or_else(|| refused("host.migration_source_ownership_missing"))?;
                    uuid(label)?;
                    let id = Uuid::parse_str(&record.id)
                        .map_err(|_| refused("host.migration_invalid_identity"))?;
                    let expected_name = format!("festerm-{}", id.simple());
                    if ownership.version != 1
                        || ownership.installation != record.id
                        || ownership.fingerprint != record.fingerprint
                        || ownership.qualified != record.tunnel
                        || target
                            .as_ref()
                            .is_some_and(|target| target.name != expected_name)
                        || record.tunnel.as_ref().is_some_and(|qualified| {
                            !qualified.rsplit_once('.').is_some_and(|(name, cluster)| {
                                name == expected_name
                                    && !cluster.is_empty()
                                    && cluster.len() <= 32
                                    && cluster.bytes().all(|byte| byte.is_ascii_alphanumeric())
                            })
                        })
                    {
                        return Err(refused("host.migration_source_receipt_mismatch"));
                    }
                    let deletion = optional::<serde_json::Value>(&directory, "deletion.json")?;
                    let legacy = optional::<serde_json::Value>(&directory, "legacy-deletion.json")?;
                    (
                        None,
                        None,
                        receipt,
                        ownership.incarnation,
                        deletion.is_none() && legacy.is_none(),
                        deletion.is_some() || legacy.is_some(),
                    )
                }
                _ => return Err(refused("host.migration_foreign_source_origin")),
            };
        // These are retained, never converted into owner/session grants.
        let _retained_metadata = (record.no_takeover, &record.filesystem, &record.observation);
        let availability = if cleanup {
            Availability::CleanupPending
        } else if !active || record.phase != "configured" || record.tunnel.is_none() {
            Availability::RevokedOrIncomplete
        } else if let Some(target) = &target {
            if (self.probe())(
                target,
                record
                    .origin
                    .as_ref()
                    .map(|origin| &origin.spawn_fingerprint),
            )? {
                Availability::AuthenticatedBindingUnavailable
            } else {
                Availability::NativeGenerationUnavailable
            }
        } else {
            Availability::RevokedOrIncomplete
        };
        Ok(Loaded {
            directory,
            profile,
            consent: consent.clone(),
            retained: RetainedSource {
                row: InventoryRow {
                    selection: selection.clone(),
                    account: account.into(),
                    original_host_id: identity.host_id(),
                    original_key_fingerprint: identity.fingerprint()?,
                    original_cloud_id: record.tunnel,
                    original_cloud_incarnation: incarnation,
                    original_consent: consent.map(|grant| grant.consent),
                    target,
                    availability,
                    live_stop_authorized: false,
                },
                installation_receipt,
                ownership_receipt,
            },
            _leases: leases,
            probe: self.probe(),
        })
    }

    fn load_all(&self, values: &[Selection], account: &str) -> io::Result<Vec<Loaded>> {
        selections(values)?;
        let mut ordered = values.to_vec();
        ordered.sort_by(|left, right| left.source.cmp(&right.source));
        let loaded: Vec<Loaded> = ordered
            .iter()
            .map(|value| self.load(value, account))
            .collect::<io::Result<_>>()?;
        let mut targets = BTreeSet::new();
        for source in &loaded {
            if let Some(target) = &source.retained.row.target {
                if !targets.insert((
                    &target.runtime_root,
                    &target.name,
                    target.pid,
                    target.generation,
                    &target.endpoint,
                )) {
                    return Err(refused("host.migration_duplicate_native_generation"));
                }
            }
        }
        Ok(loaded)
    }

    pub fn inventory(&self, values: &[Selection], account: &str) -> io::Result<Vec<InventoryRow>> {
        let rows = self.load_all(values, account).map(|sources| {
            sources
                .into_iter()
                .map(|source| source.retained.row)
                .collect::<Vec<_>>()
        })?;
        #[cfg(feature = "owner-enrollment")]
        let rows = {
            let mut rows = rows;
            self.negotiate_preview(&mut rows)?;
            rows
        };
        Ok(rows)
    }

    pub fn preview(
        &self,
        destination: &Store,
        values: &[Selection],
        account: &str,
    ) -> io::Result<Preview> {
        let sources = self.load_all(values, account)?;
        let preview = destination.with_locked(|_, snapshot| {
            reservable(snapshot)?;
            let plan = plan(snapshot, account, &sources);
            Ok(Preview {
                preview_id: digest(&plan)?,
                destination: snapshot.clone(),
                account: account.into(),
                mappings: sources
                    .iter()
                    .map(|source| source.retained.row.clone())
                    .collect(),
                daemon_binding_supported: false,
            })
        })?;
        drop(sources);
        #[cfg(feature = "owner-enrollment")]
        let preview = {
            let mut preview = preview;
            preview.daemon_binding_supported = self.negotiate_preview(&mut preview.mappings)?;
            preview
        };
        Ok(preview)
    }

    /// Explicit reservation only: no provisioning, enrollment, frontend, stop or
    /// permission import. Retry the original preview or use `resume`.
    pub fn reserve(
        &self,
        destination: &Store,
        values: &[Selection],
        account: &str,
        approved_preview: &str,
    ) -> io::Result<Journal> {
        let sources = self.load_all(values, account)?;
        with_permits(&sources, &mut || {
            destination.with_locked(|directory, snapshot| {
                reservable(snapshot)?;
                let plan = plan(snapshot, account, &sources);
                if digest(&plan)? != approved_preview {
                    return Err(refused("host.migration_preview_changed_reconfirm"));
                }
                let intent = match optional::<Intent>(directory, INTENT)? {
                    Some(intent) => {
                        validate_intent(&intent, snapshot)?;
                        if intent.plan != plan || intent.preview_id != approved_preview {
                            return Err(refused("host.migration_existing_reservation"));
                        }
                        intent
                    }
                    None => {
                        if optional::<Journal>(directory, JOURNAL)?.is_some() {
                            return Err(refused("host.migration_existing_identity_or_orphan"));
                        }
                        // Any non-NotFound identity error must remain a refusal.
                        match storage::open_private_file(directory, Path::new(HOST_IDENTITY), false)
                        {
                            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                            Err(error) => return Err(error),
                            Ok(_) => {
                                return Err(refused("host.migration_existing_identity_or_orphan"))
                            }
                        }
                        let intent = Intent {
                            version: 1,
                            operation: Uuid::new_v4().to_string(),
                            preview_id: approved_preview.into(),
                            plan,
                        };
                        protected_state::publish_new(
                            directory,
                            Path::new(INTENT),
                            &encode(&intent)?,
                        )?;
                        intent
                    }
                };
                finish_reservation(directory, snapshot, &intent, &sources)
            })
        })
    }

    /// Revalidates the original exact source bytes, namespace and destination
    /// epoch. It never refreshes an old operation into a new reservation.
    pub fn resume(&self, destination: &Store) -> io::Result<Journal> {
        let intent = destination.with_locked(|directory, snapshot| {
            let intent: Intent = read(directory, INTENT, LIMIT)?;
            validate_intent(&intent, snapshot)?;
            Ok(intent)
        })?;
        let values: Vec<_> = intent
            .plan
            .sources
            .iter()
            .map(|source| source.row.selection.clone())
            .collect();
        let sources = self.load_all(&values, &intent.plan.account)?;
        with_permits(&sources, &mut || {
            destination.with_locked(|directory, snapshot| {
                validate_intent(&intent, snapshot)?;
                if plan(snapshot, &intent.plan.account, &sources) != intent.plan {
                    return Err(refused("host.migration_source_changed_preserve_legacy"));
                }
                finish_reservation(directory, snapshot, &intent, &sources)
            })
        })
    }

    pub fn defer(&self, destination: &Store) -> io::Result<Journal> {
        #[cfg(feature = "owner-enrollment")]
        if binding::has_bindings(destination)? {
            return binding::defer_existing(destination);
        }
        let expected = self.resume(destination)?;
        destination.with_locked(|directory, snapshot| {
            let intent: Intent = read(directory, INTENT, LIMIT)?;
            validate_intent(&intent, snapshot)?;
            let mut journal = read_journal(directory, &intent)?;
            if journal != expected {
                return Err(refused("host.migration_stale_operation"));
            }
            journal.phase = Phase::Deferred;
            save_journal(directory, &journal)?;
            Ok(journal)
        })
    }

    /// Releases only this operation's local claims. Never restores consent,
    /// deletes a cloud resource/key or stops/recreates a daemon.
    pub fn rollback(&self, destination: &Store) -> io::Result<Journal> {
        destination.with_locked(|directory, snapshot| {
            let intent: Intent = read(directory, INTENT, LIMIT)?;
            validate_intent_identity(&intent, snapshot)?;
            #[cfg(feature = "owner-enrollment")]
            binding::all_released(directory, &intent)?;
            #[cfg(not(feature = "owner-enrollment"))]
            if optional::<serde_json::Value>(directory, "host-migration-bindings.json")?.is_some() {
                return Err(refused("host.migration_exact_native_release_required"));
            }
            if !matches!(
                snapshot.phase,
                HostPhase::Disabled | HostPhase::StopRequested | HostPhase::CleanupPending
            ) {
                return Err(refused(
                    "host.migration_disable_destination_before_rollback",
                ));
            }
            let identity = load_identity(directory)?;
            if let Some(binding) = &snapshot.binding {
                let identity = identity
                    .as_ref()
                    .ok_or_else(|| refused("host.migration_identity_missing"))?;
                if binding.host_id != identity.host_id()
                    || binding.host_key_fingerprint != identity.fingerprint()?
                    || binding.account != intent.plan.account
                {
                    return Err(refused("host.migration_destination_binding_changed"));
                }
            }
            let mut journal = match optional::<Journal>(directory, JOURNAL)? {
                Some(_) => read_journal(directory, &intent)?,
                None => new_journal(&intent, identity.as_ref())?,
            };
            if journal.phase == Phase::RolledBack {
                return Ok(journal);
            }
            journal.phase = Phase::RollbackPending;
            save_journal(directory, &journal)?;
            for retained in &intent.plan.sources {
                let source = &retained.row.selection.source;
                if journal.released_sources.contains(source) {
                    continue;
                }
                let (source_directory, _leases) = self.directory(source)?;
                let source_identity = HostIdentity::load_from_private_directory(
                    &source_directory,
                    Path::new("host-identity.key"),
                )?;
                let current: LegacyInstallation =
                    read(&source_directory, "installation.json", SOURCE_LIMIT)?;
                let origin_matches = match (source, current.version, &current.origin) {
                    (Source::DesktopProfile { .. }, 2, Some(origin)) => {
                        origin.kind == "desktop-profile"
                    }
                    (Source::StandaloneScript { .. }, 1, None) => true,
                    _ => false,
                };
                if source_identity.host_id() != retained.row.original_host_id
                    || source_identity.fingerprint()? != retained.row.original_key_fingerprint
                    || !origin_matches
                    || current.id != source.installation()
                    || current.account != intent.plan.account
                    || current.fingerprint != retained.row.original_key_fingerprint
                {
                    return Err(refused("host.migration_foreign_source_during_rollback"));
                }
                match optional::<Claim>(&source_directory, CLAIM)? {
                    Some(mut current) => {
                        validate_claim(&current)?;
                        if current.operation != intent.operation
                            && !journal.claimed_sources.contains(source)
                        {
                            journal.released_sources.insert(source.clone());
                            save_journal(directory, &journal)?;
                            continue;
                        }
                        let expected = claim(&intent, &journal, retained)?;
                        if current != expected
                            && current
                                != (Claim {
                                    released: true,
                                    ..expected.clone()
                                })
                        {
                            return Err(refused("host.migration_foreign_or_replaced_claim"));
                        }
                        current.released = true;
                        storage::replace_private(
                            &source_directory,
                            Path::new(CLAIM),
                            &encode(&current)?,
                        )?;
                    }
                    None => {
                        if journal.claimed_sources.contains(source) {
                            return Err(refused("host.migration_release_evidence_missing"));
                        }
                    }
                }
                journal.released_sources.insert(source.clone());
                save_journal(directory, &journal)?;
            }
            journal.phase = Phase::RolledBack;
            save_journal(directory, &journal)?;
            Ok(journal)
        })
    }
}

#[derive(Deserialize)]
struct NativeRegistry {
    sessions: std::collections::BTreeMap<String, NativeEvidence>,
}

#[derive(Deserialize)]
struct NativeEvidence {
    name: String,
    pid: u32,
    created_at_unix_ms: u128,
    socket: String,
    #[serde(default)]
    spawn_fingerprint: Option<String>,
}

fn exact_native(
    target: &profile_hosting::Target,
    fingerprint: Option<&String>,
) -> io::Result<bool> {
    let suffix = format!("{}-{}", target.pid, target.generation);
    if !(target.endpoint.ends_with(&suffix) || target.endpoint.ends_with(&format!("{suffix}.sock")))
    {
        return Ok(false);
    }
    let directory = storage::private_directory(&target.runtime_root, false)?;
    let lock = storage::open_private_file(&directory, Path::new("registry.lock"), false)?;
    FileExt::try_lock_shared(&lock)?;
    let registry: NativeRegistry = read(&directory, "registry.json", LIMIT)?;
    let Some(session) = registry.sessions.get(&target.name) else {
        return Ok(false);
    };
    if session.name != target.name
        || session.pid != target.pid
        || session.created_at_unix_ms != target.generation
        || session.socket != target.endpoint
    {
        return Err(refused("host.migration_native_generation_replaced"));
    }
    let Some(evidence) = &session.spawn_fingerprint else {
        return Ok(false);
    };
    if fingerprint.is_some_and(|expected| expected != evidence) {
        return Err(refused("host.migration_native_evidence_untrusted"));
    }
    let evidence = evidence.clone();
    drop(lock);
    // Both origins use the private protocol/endpoint/lease-backed verifier.
    // Legacy inventory's PID-only compatibility is deliberately not proof.
    festerm_sessiond::verify_spawn_fingerprint_in(
        &target.runtime_root,
        &NativeSessionIdentity {
            name: target.name.clone(),
            pid: target.pid,
            created_at_unix_ms: target.generation,
            endpoint: target.endpoint.clone(),
        },
        &evidence,
    )
    .map_err(|_| refused("host.migration_native_evidence_untrusted"))?;
    Ok(true)
}

fn with_permits<T>(
    sources: &[Loaded],
    action: &mut impl FnMut() -> io::Result<T>,
) -> io::Result<T> {
    let Some((source, rest)) = sources.split_first() else {
        return action();
    };
    if source.retained.row.availability != Availability::AuthenticatedBindingUnavailable {
        return Err(refused("host.migration_source_not_reservable"));
    }
    let check = &mut || {
        if raw(&source.directory, "installation.json")? != source.retained.installation_receipt {
            return Err(refused("host.migration_source_changed_preserve_legacy"));
        }
        let name = if source.profile.is_some() {
            "profile-consent.json"
        } else {
            "script-ownership.json"
        };
        if raw(&source.directory, name)? != source.retained.ownership_receipt {
            return Err(refused("host.migration_source_changed_preserve_legacy"));
        }
        let target = source
            .retained
            .row
            .target
            .as_ref()
            .ok_or_else(|| refused("host.migration_target_missing"))?;
        if !(source.probe)(
            target,
            source
                .consent
                .as_ref()
                .map(|grant| &grant.spawn_fingerprint),
        )? {
            return Err(refused("host.migration_native_generation_unavailable"));
        }
        with_permits(rest, action)
    };
    if let (Some(profile), Some(grant)) = (&source.profile, &source.consent) {
        profile.with_hosting_permit(
            &grant.consent,
            source
                .retained
                .row
                .target
                .as_ref()
                .ok_or_else(|| refused("host.migration_target_missing"))?,
            source
                .retained
                .row
                .original_cloud_id
                .as_deref()
                .ok_or_else(|| refused("host.migration_source_ownership_missing"))?,
            &grant
                .cloud_reservation
                .as_ref()
                .ok_or_else(|| refused("host.migration_source_ownership_missing"))?
                .ownership_label,
            &grant.spawn_fingerprint,
            check,
        )
    } else {
        check()
    }
}

fn reservable(snapshot: &Snapshot) -> io::Result<()> {
    if snapshot.phase != HostPhase::Disabled
        || snapshot.epoch != 0
        || snapshot.binding.is_some()
        || snapshot.pending_ticket().is_some()
    {
        return Err(refused(
            "host.migration_requires_fresh_disabled_destination",
        ));
    }
    Ok(())
}

fn plan(snapshot: &Snapshot, account: &str, sources: &[Loaded]) -> Plan {
    Plan {
        destination: snapshot.clone(),
        account: account.into(),
        sources: sources
            .iter()
            .map(|source| source.retained.clone())
            .collect(),
    }
}

fn validate_intent_identity(intent: &Intent, snapshot: &Snapshot) -> io::Result<()> {
    uuid(&intent.operation)?;
    if intent.version != 1
        || intent.plan.destination.installation != snapshot.installation
        || intent.plan.destination.origin != snapshot.origin
        || intent.preview_id != digest(&intent.plan)?
    {
        return Err(refused("host.migration_foreign_or_corrupt_intent"));
    }
    let values: Vec<_> = intent
        .plan
        .sources
        .iter()
        .map(|source| source.row.selection.clone())
        .collect();
    selections(&values)?;
    for source in &intent.plan.sources {
        uuid(&source.row.original_host_id)?;
        if let Some(consent) = &source.row.original_consent {
            uuid(consent)?;
        }
        if source.row.account != intent.plan.account
            || source.row.original_key_fingerprint.is_empty()
            || source.row.live_stop_authorized
            || source.row.availability != Availability::AuthenticatedBindingUnavailable
            || source.installation_receipt.len() > SOURCE_LIMIT
            || source.ownership_receipt.len() > SOURCE_LIMIT
        {
            return Err(refused("host.migration_foreign_or_corrupt_intent"));
        }
    }
    Ok(())
}

fn validate_intent(intent: &Intent, snapshot: &Snapshot) -> io::Result<()> {
    validate_intent_identity(intent, snapshot)?;
    reservable(snapshot)?;
    if intent.plan.destination != *snapshot {
        return Err(refused(
            "host.migration_stale_destination_epoch_or_settings",
        ));
    }
    Ok(())
}

fn load_identity(directory: &File) -> io::Result<Option<HostIdentity>> {
    match HostIdentity::load_from_private_directory(directory, Path::new(HOST_IDENTITY)) {
        Ok(identity) => Ok(Some(identity)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn new_journal(intent: &Intent, identity: Option<&HostIdentity>) -> io::Result<Journal> {
    Ok(Journal {
        version: 1,
        operation: intent.operation.clone(),
        preview_id: intent.preview_id.clone(),
        destination_installation: intent.plan.destination.installation.clone(),
        destination_host_id: identity.map(HostIdentity::host_id),
        destination_key_fingerprint: identity.map(HostIdentity::fingerprint).transpose()?,
        account: intent.plan.account.clone(),
        phase: Phase::Reserving,
        claimed_sources: BTreeSet::new(),
        released_sources: BTreeSet::new(),
    })
}

fn read_journal(directory: &File, intent: &Intent) -> io::Result<Journal> {
    let journal: Journal = read(directory, JOURNAL, LIMIT)?;
    let identity = load_identity(directory)?;
    let expected = if journal.phase == Phase::RolledBack
        && journal.destination_host_id.is_none()
        && journal.destination_key_fingerprint.is_none()
        && journal.claimed_sources.is_empty()
    {
        // A pre-identity abort is history, not a pin for an independent later
        // ordinary setup. It can never become a resumable migration again.
        new_journal(intent, None)?
    } else {
        new_journal(intent, identity.as_ref())?
    };
    if (Journal {
        phase: expected.phase,
        claimed_sources: BTreeSet::new(),
        released_sources: BTreeSet::new(),
        ..journal.clone()
    }) != expected
        || !journal.claimed_sources.iter().all(|source| {
            intent
                .plan
                .sources
                .iter()
                .any(|retained| &retained.row.selection.source == source)
        })
        || !journal.released_sources.iter().all(|source| {
            intent
                .plan
                .sources
                .iter()
                .any(|retained| &retained.row.selection.source == source)
        })
        || matches!(
            journal.phase,
            Phase::Reserving
                | Phase::AwaitingNativeBinding
                | Phase::Deferred
                | Phase::Bound
                | Phase::Switched
        ) && (!journal.released_sources.is_empty() || identity.is_none())
        || matches!(
            journal.phase,
            Phase::AwaitingNativeBinding | Phase::Deferred | Phase::Bound | Phase::Switched
        ) && journal.claimed_sources.len() != intent.plan.sources.len()
        || journal.phase == Phase::RolledBack
            && journal.released_sources.len() != intent.plan.sources.len()
    {
        return Err(refused("host.migration_foreign_or_corrupt_journal"));
    }
    Ok(journal)
}

fn save_journal(directory: &File, journal: &Journal) -> io::Result<()> {
    storage::replace_private(directory, Path::new(JOURNAL), &encode(journal)?)
}

fn claim(intent: &Intent, journal: &Journal, source: &RetainedSource) -> io::Result<Claim> {
    Ok(Claim {
        version: 1,
        source: source.row.selection.source.clone(),
        source_host_id: source.row.original_host_id.clone(),
        source_key_fingerprint: source.row.original_key_fingerprint.clone(),
        source_receipt_digest: digest(source)?,
        destination_installation: journal.destination_installation.clone(),
        destination_host_id: journal
            .destination_host_id
            .clone()
            .ok_or_else(|| refused("host.migration_identity_missing"))?,
        destination_key_fingerprint: journal
            .destination_key_fingerprint
            .clone()
            .ok_or_else(|| refused("host.migration_identity_missing"))?,
        operation: intent.operation.clone(),
        released: false,
    })
}

fn validate_claim(claim: &Claim) -> io::Result<()> {
    uuid(claim.source.installation())?;
    uuid(&claim.source_host_id)?;
    uuid(&claim.destination_installation)?;
    uuid(&claim.destination_host_id)?;
    uuid(&claim.operation)?;
    if claim.version != 1
        || claim.source_key_fingerprint.is_empty()
        || claim.destination_key_fingerprint.is_empty()
        || claim.source_receipt_digest.len() != 64
        || !claim
            .source_receipt_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(refused("host.migration_foreign_or_corrupt_claim"));
    }
    Ok(())
}

fn finish_reservation(
    directory: &File,
    snapshot: &Snapshot,
    intent: &Intent,
    sources: &[Loaded],
) -> io::Result<Journal> {
    validate_intent(intent, snapshot)?;
    let mut journal = match optional::<Journal>(directory, JOURNAL)? {
        Some(_) => read_journal(directory, intent)?,
        None => {
            for source in sources {
                if optional::<Claim>(&source.directory, CLAIM)?
                    .is_some_and(|claim| claim.operation == intent.operation)
                {
                    return Err(refused("host.migration_journal_evidence_missing"));
                }
            }
            let identity = HostIdentity::load_or_create_from_private_directory(
                directory,
                Path::new(HOST_IDENTITY),
            )?;
            let journal = new_journal(intent, Some(&identity))?;
            protected_state::publish_new(directory, Path::new(JOURNAL), &encode(&journal)?)?;
            journal
        }
    };
    if !matches!(
        journal.phase,
        Phase::Reserving | Phase::AwaitingNativeBinding | Phase::Deferred
    ) {
        return Err(refused("host.migration_operation_rolled_back"));
    }
    for source in sources {
        let expected = claim(intent, &journal, &source.retained)?;
        match optional::<Claim>(&source.directory, CLAIM)? {
            Some(current) if current == expected => {}
            Some(current)
                if current.released
                    && current.operation != intent.operation
                    && current.source == expected.source
                    && current.source_host_id == expected.source_host_id
                    && current.source_key_fingerprint == expected.source_key_fingerprint =>
            {
                validate_claim(&current)?;
                if journal.claimed_sources.contains(&expected.source) {
                    return Err(refused("host.migration_replaced_claim"));
                }
                storage::replace_private(&source.directory, Path::new(CLAIM), &encode(&expected)?)?;
            }
            Some(_) => return Err(refused("host.migration_source_already_reserved_or_revoked")),
            None => {
                if journal.claimed_sources.contains(&expected.source) {
                    return Err(refused("host.migration_claim_evidence_missing"));
                }
                protected_state::publish_new(
                    &source.directory,
                    Path::new(CLAIM),
                    &encode(&expected)?,
                )?;
            }
        }
        journal.claimed_sources.insert(expected.source);
        save_journal(directory, &journal)?;
    }
    if journal.phase == Phase::Reserving {
        journal.phase = Phase::AwaitingNativeBinding;
        save_journal(directory, &journal)?;
    }
    Ok(journal)
}

/// Read history even if a source is now offline/revoked. This is not a resume.
pub fn status(destination: &Store) -> io::Result<Journal> {
    destination.with_locked(|directory, snapshot| {
        let intent: Intent = read(directory, INTENT, LIMIT)?;
        validate_intent_identity(&intent, snapshot)?;
        read_journal(directory, &intent)
    })
}

pub(crate) fn admit_enable(directory: &File, snapshot: &Snapshot) -> io::Result<()> {
    match optional::<Intent>(directory, INTENT)? {
        Some(intent) => {
            validate_intent_identity(&intent, snapshot)?;
            let phase = read_journal(directory, &intent)?.phase;
            #[cfg(feature = "owner-enrollment")]
            if phase == Phase::Switched {
                binding::require_switched(directory, &intent)?;
                return Ok(());
            }
            if phase != Phase::RolledBack {
                return Err(refused(
                    "host.migration_native_binding_required_or_rollback",
                ));
            }
        }
        None => {
            if optional::<Journal>(directory, JOURNAL)?.is_some() {
                return Err(refused("host.migration_orphan_journal"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
