use super::*;
use crate::enrollment::{MigrationOwnerPolicy, OwnerStorage};
use festerm_host_protocol::Epoch;
use festerm_sessiond::{
    host_control::{ControlError, SessionControl},
    native_binding::{Binding, BindingPhase},
};
use std::{collections::BTreeMap, path::PathBuf};

const BINDINGS: &str = "host-migration-bindings.json";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingJournal {
    version: u8,
    pub operation: String,
    pub owner_storage: OwnerStorage,
    pub bindings: BTreeMap<String, Binding>,
    pub bound: BTreeSet<String>,
    pub switched: BTreeSet<String>,
    pub released: BTreeSet<String>,
    pub switch_requested: bool,
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn control_error(error: ControlError) -> io::Error {
    io::Error::new(
        if error == ControlError::Busy {
            io::ErrorKind::WouldBlock
        } else {
            io::ErrorKind::Other
        },
        match error {
            ControlError::CommittedButNotPublished => {
                "host.migration_binding_uncertain_resume_same_operation"
            }
            ControlError::Busy => "host.migration_binding_busy",
            ControlError::Revoked => "host.migration_binding_revoked",
            ControlError::Stale => "host.migration_binding_stale",
            _ => "host.migration_binding_refused_preserve_legacy",
        },
    )
}

fn authorization_error(error: io::Error) -> ControlError {
    if error.kind() == io::ErrorKind::WouldBlock {
        ControlError::Busy
    } else {
        ControlError::Denied
    }
}

pub(crate) fn check_migration_owner(
    directory: &File,
    snapshot: &Snapshot,
    operation: &str,
    account: &str,
    reduction: bool,
) -> io::Result<()> {
    let intent: Intent = read(directory, INTENT, LIMIT)?;
    validate_intent_identity(&intent, snapshot)?;
    let journal = read_journal(directory, &intent)?;
    if intent.operation != operation
        || intent.plan.account != account
        || journal.claimed_sources.len() != intent.plan.sources.len()
        || matches!(journal.phase, Phase::RollbackPending | Phase::RolledBack) && !reduction
    {
        return Err(refused("host.migration_owner_operation_or_account_changed"));
    }
    if !reduction {
        validate_intent(&intent, snapshot)?;
    }
    if let Some(binding) = &snapshot.binding {
        if binding.account != account
            || Some(&binding.host_id) != journal.destination_host_id.as_ref()
            || Some(&binding.host_key_fingerprint) != journal.destination_key_fingerprint.as_ref()
        {
            return Err(refused("host.migration_foreign_destination_binding"));
        }
    }
    Ok(())
}

pub(crate) fn check_owner_initialization(
    directory: &File,
    snapshot: &Snapshot,
    operation: &str,
    account: &str,
) -> io::Result<()> {
    check_migration_owner(directory, snapshot, operation, account, false)?;
    let journal: Journal = read(directory, JOURNAL, LIMIT)?;
    if !matches!(
        journal.phase,
        Phase::AwaitingNativeBinding | Phase::Deferred
    ) {
        return Err(refused("host.migration_owner_initialization_not_current"));
    }
    Ok(())
}

fn load(directory: &File, intent: &Intent) -> io::Result<BindingJournal> {
    let journal: BindingJournal = read(directory, BINDINGS, LIMIT)?;
    let reservation = read_journal(directory, intent)?;
    if journal.version != 1
        || journal.operation != intent.operation
        || journal.bindings.len() != intent.plan.sources.len()
        || !journal
            .bound
            .is_subset(&journal.bindings.keys().cloned().collect())
        || !journal.switched.is_subset(&journal.bound)
        || !journal.switched.is_empty() && !journal.switch_requested
        || !journal
            .released
            .is_subset(&journal.bindings.keys().cloned().collect())
    {
        return Err(refused("host.migration_corrupt_binding_journal"));
    }
    for source in &intent.plan.sources {
        let binding = journal
            .bindings
            .get(&source.row.selection.logical_id)
            .ok_or_else(|| refused("host.migration_binding_mapping_missing"))?;
        binding.validate()?;
        let target = source
            .row
            .target
            .as_ref()
            .ok_or_else(|| refused("host.migration_target_missing"))?;
        if binding.operation.as_str() != intent.operation
            || Some(binding.host.as_str()) != reservation.destination_host_id.as_deref()
            || Some(binding.host_key_fingerprint.as_str())
                != reservation.destination_key_fingerprint.as_deref()
            || binding.installation != intent.plan.destination.installation
            || binding.account != intent.plan.account
            || binding.source_installation != source.row.selection.source.installation()
            || binding.source_host.as_str() != source.row.original_host_id
            || binding.source_key_fingerprint != source.row.original_key_fingerprint
            || binding.source_receipt_digest != digest(source)?
            || binding.source_installation_digest != hash(source.installation_receipt.as_bytes())
            || binding.source_ownership_digest != hash(source.ownership_receipt.as_bytes())
            || binding.logical.as_str() != source.row.selection.logical_id
            || binding.runtime_root != target.runtime_root
            || binding.identity != native_identity(target)
        {
            return Err(refused("host.migration_foreign_binding_journal"));
        }
    }
    Ok(journal)
}

fn active_journal(directory: &File, intent: &Intent) -> io::Result<Journal> {
    let journal = read_journal(directory, intent)?;
    if matches!(journal.phase, Phase::RollbackPending | Phase::RolledBack) {
        return Err(refused("host.migration_operation_rolled_back"));
    }
    Ok(journal)
}

fn native_identity(target: &profile_hosting::Target) -> NativeSessionIdentity {
    NativeSessionIdentity {
        name: target.name.clone(),
        pid: target.pid,
        created_at_unix_ms: target.generation,
        endpoint: target.endpoint.clone(),
    }
}

pub(super) fn preview_native(rows: &mut [InventoryRow]) -> io::Result<bool> {
    let mut supported = true;
    for row in rows {
        if row.availability != Availability::AuthenticatedBindingUnavailable {
            supported = false;
            continue;
        }
        let target = row
            .target
            .as_ref()
            .ok_or_else(|| refused("host.migration_target_missing"))?;
        let directory = storage::private_directory(&target.runtime_root, false)?;
        let registry: NativeRegistry = read(&directory, "registry.json", LIMIT)?;
        let fingerprint = registry
            .sessions
            .get(&target.name)
            .and_then(|native| native.spawn_fingerprint.as_deref())
            .ok_or_else(|| refused("host.migration_native_proof_missing"))?;
        match festerm_sessiond::native_binding::probe(
            &target.runtime_root,
            &native_identity(target),
            fingerprint,
        ) {
            Ok(None) => row.availability = Availability::AuthenticatedBindingAvailable,
            Ok(Some(_)) => {
                row.availability = Availability::AlreadyBound;
                supported = false;
            }
            Err(error) if error.kind() == io::ErrorKind::Unsupported => supported = false,
            Err(error) => return Err(error),
        }
    }
    Ok(supported)
}

fn control(store: &Store, state: &Path) -> io::Result<SessionControl> {
    let selected = storage::private_directory(state, false)?;
    store.with_locked(|directory, _| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let actual = directory.metadata()?;
            let proposed = selected.metadata()?;
            if (actual.dev(), actual.ino()) != (proposed.dev(), proposed.ino()) {
                return Err(refused("host.migration_foreign_state_namespace"));
            }
        }
        #[cfg(windows)]
        if !festerm_windows_security::same_file_identity(directory, &selected)? {
            return Err(refused("host.migration_foreign_state_namespace"));
        }
        Ok(())
    })?;
    if has_bindings(store)? {
        let directory = storage::private_directory(&state.join("daemon"), false)?;
        storage::read_private(&directory, Path::new("host-sessions.json"), LIMIT)?;
        storage::open_private_file(&directory, Path::new("host-sessions.lock"), false)?;
        storage::read_private(&directory, Path::new("host-binding-namespace.json"), 256)?;
    }
    SessionControl::existing_only(&state.join("daemon")).map_err(control_error)
}

struct BindingGuard {
    _sources: Vec<Loaded>,
    _permits: Vec<profile_hosting::HostingPermit>,
    _owner: MigrationOwnerPolicy,
}

impl Inventory {
    fn source_root(&self, source: &Source) -> PathBuf {
        match source {
            Source::DesktopProfile { installation } => {
                self.desktop.join("installations").join(installation)
            }
            Source::StandaloneScript { .. } => self.script.join("state"),
        }
    }

    fn binding_guard(
        &self,
        destination: &Store,
        binding: &Binding,
        storage: OwnerStorage,
    ) -> io::Result<BindingGuard> {
        let intent = destination.with_locked(|directory, snapshot| {
            let intent: Intent = read(directory, INTENT, LIMIT)?;
            validate_intent(&intent, snapshot)?;
            let bindings = load(directory, &intent)?;
            if bindings.owner_storage != storage
                || bindings.bindings.get(binding.logical.as_str()) != Some(binding)
                || !bindings.released.is_empty()
            {
                return Err(refused("host.migration_stale_binding_operation"));
            }
            Ok(intent)
        })?;
        let values = intent
            .plan
            .sources
            .iter()
            .map(|source| source.row.selection.clone())
            .collect::<Vec<_>>();
        let sources = self.load_all(&values, &intent.plan.account)?;
        let mut permits = Vec::new();
        for source in &sources {
            if let (Some(profile), Some(grant)) = (&source.profile, &source.consent) {
                let cloud = grant
                    .cloud_reservation
                    .as_ref()
                    .ok_or_else(|| refused("host.migration_source_ownership_missing"))?;
                permits.push(
                    profile.try_hosting_permit(
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
                        &cloud.ownership_label,
                        &grant.spawn_fingerprint,
                    )?,
                );
            }
        }
        let owner = destination
            .owner_enrollment_with_storage(storage)?
            .migration_policy(&intent.operation, false)?;
        if plan(&owner.installation, &intent.plan.account, &sources) != intent.plan {
            return Err(refused("host.migration_source_changed_preserve_legacy"));
        }
        let enrollment = &owner.enrollment;
        if enrollment.binding.host.host != binding.host
            || enrollment.binding.host.host_key_fingerprint != binding.host_key_fingerprint
            || enrollment.binding.principal != binding.principal
            || enrollment.binding.account != binding.account
            || enrollment.revision != binding.owner_epoch
            || Epoch::new(
                owner
                    .installation
                    .epoch
                    .checked_add(1)
                    .ok_or_else(|| refused("host.migration_epoch_exhausted"))?,
            )? != binding.host_epoch
        {
            return Err(refused("host.migration_current_owner_changed"));
        }
        let selected = sources
            .iter()
            .find(|source| source.retained.row.selection.logical_id == binding.logical.as_str())
            .ok_or_else(|| refused("host.migration_unknown_mapping"))?;
        if binding.source_root != self.source_root(&selected.retained.row.selection.source)
            || binding.source_profile != selected.profile.is_some()
            || binding.source_key_digest
                != hash(&storage::read_private(
                    &selected.directory,
                    Path::new("host-identity.key"),
                    SOURCE_LIMIT,
                )?)
            || binding.source_claim_digest
                != hash(&storage::read_private(
                    &selected.directory,
                    Path::new(CLAIM),
                    SOURCE_LIMIT,
                )?)
        {
            return Err(refused("host.migration_source_namespace_or_key_changed"));
        }
        destination_claims(&sources, &intent, &owner)?;
        Ok(BindingGuard {
            _sources: sources,
            _permits: permits,
            _owner: owner,
        })
    }

    pub fn bind_existing(
        &self,
        destination: &Store,
        state: &Path,
        storage_mode: OwnerStorage,
    ) -> io::Result<BindingJournal> {
        let (intent, existing) = destination.with_locked(|directory, snapshot| {
            let intent: Intent = read(directory, INTENT, LIMIT)?;
            validate_intent(&intent, snapshot)?;
            active_journal(directory, &intent)?;
            let existing = match optional::<BindingJournal>(directory, BINDINGS)? {
                Some(_) => Some(load(directory, &intent)?),
                None => None,
            };
            Ok((intent, existing))
        })?;
        let sessions = control(destination, state)?;
        if existing.is_none() {
            if sessions.has_binding_records().map_err(control_error)? {
                return Err(refused("host.migration_binding_journal_missing"));
            }
            let values = intent
                .plan
                .sources
                .iter()
                .map(|source| source.row.selection.clone())
                .collect::<Vec<_>>();
            let sources = self.load_all(&values, &intent.plan.account)?;
            let owner = destination
                .owner_enrollment_with_storage(storage_mode)?
                .migration_policy(&intent.operation, false)?;
            if plan(&owner.installation, &intent.plan.account, &sources) != intent.plan {
                return Err(refused("host.migration_source_changed_preserve_legacy"));
            }
            let (namespace, issuer) = sessions.binding_namespace().map_err(control_error)?;
            let mut bindings = BTreeMap::new();
            for source in &sources {
                let row = &source.retained.row;
                let target = row
                    .target
                    .as_ref()
                    .ok_or_else(|| refused("host.migration_target_missing"))?;
                let runtime = storage::private_directory(&target.runtime_root, false)?;
                let registry: NativeRegistry = read(&runtime, "registry.json", LIMIT)?;
                let fingerprint = registry
                    .sessions
                    .get(&target.name)
                    .and_then(|native| native.spawn_fingerprint.clone())
                    .ok_or_else(|| refused("host.migration_native_proof_missing"))?;
                let binding = Binding {
                    operation: intent.operation.clone().try_into()?,
                    host: owner.enrollment.binding.host.host.clone(),
                    installation: owner.installation.installation.clone(),
                    account: intent.plan.account.clone(),
                    host_key_fingerprint: owner
                        .enrollment
                        .binding
                        .host
                        .host_key_fingerprint
                        .clone(),
                    principal: owner.enrollment.binding.principal.clone(),
                    owner_epoch: owner.enrollment.revision,
                    host_epoch: Epoch::new(
                        owner
                            .installation
                            .epoch
                            .checked_add(1)
                            .ok_or_else(|| refused("host.migration_epoch_exhausted"))?,
                    )?,
                    issuance_epoch: issuer,
                    logical: row.selection.logical_id.clone().try_into()?,
                    source_authority: Default::default(),
                    namespace: namespace.clone(),
                    control_root: state.join("daemon"),
                    source_installation: row.selection.source.installation().into(),
                    source_host: row.original_host_id.clone().try_into()?,
                    source_key_fingerprint: row.original_key_fingerprint.clone(),
                    source_receipt_digest: digest(&source.retained)?,
                    source_root: self.source_root(&row.selection.source),
                    source_profile: source.profile.is_some(),
                    source_installation_digest: hash(
                        source.retained.installation_receipt.as_bytes(),
                    ),
                    source_ownership_digest: hash(source.retained.ownership_receipt.as_bytes()),
                    source_key_digest: hash(&storage::read_private(
                        &source.directory,
                        Path::new("host-identity.key"),
                        SOURCE_LIMIT,
                    )?),
                    source_claim_digest: hash(&storage::read_private(
                        &source.directory,
                        Path::new(CLAIM),
                        SOURCE_LIMIT,
                    )?),
                    runtime_root: target.runtime_root.clone(),
                    identity: native_identity(target),
                    spawn_fingerprint: fingerprint,
                };
                binding.validate()?;
                bindings.insert(row.selection.logical_id.clone(), binding);
            }
            drop(owner);
            drop(sources);
            for binding in bindings.values() {
                let receipt = festerm_sessiond::native_binding::probe(
                    &binding.runtime_root,
                    &binding.identity,
                    &binding.spawn_fingerprint,
                )?;
                if receipt.as_ref().is_some_and(|receipt| {
                    receipt.binding != *binding
                        || matches!(
                            receipt.phase,
                            BindingPhase::Released | BindingPhase::Revoked
                        )
                }) {
                    return Err(refused("host.migration_native_binding_foreign_or_released"));
                }
            }
            destination.with_locked(|directory, snapshot| {
                validate_intent(&intent, snapshot)?;
                active_journal(directory, &intent)?;
                let journal = BindingJournal {
                    version: 1,
                    operation: intent.operation.clone(),
                    owner_storage: storage_mode,
                    bindings,
                    bound: BTreeSet::new(),
                    switched: BTreeSet::new(),
                    released: BTreeSet::new(),
                    switch_requested: false,
                };
                protected_state::publish_new(directory, Path::new(BINDINGS), &encode(&journal)?)
            })?;
        }
        let current = binding_status(destination)?;
        if current.owner_storage != storage_mode || !current.released.is_empty() {
            return Err(refused(
                "host.migration_binding_storage_or_operation_changed",
            ));
        }
        for (logical, binding) in &current.bindings {
            sessions
                .bind_existing(binding, |binding| {
                    self.binding_guard(destination, binding, storage_mode)
                        .map_err(authorization_error)
                })
                .map_err(control_error)?;
            checkpoint(destination, logical, BindingPhase::Prepared)?;
        }
        destination.with_locked(|directory, snapshot| {
            let intent: Intent = read(directory, INTENT, LIMIT)?;
            validate_intent(&intent, snapshot)?;
            let mut journal = active_journal(directory, &intent)?;
            if journal.phase != Phase::Switched {
                journal.phase = Phase::Bound;
            }
            save_journal(directory, &journal)
        })?;
        binding_status(destination)
    }

    pub fn switch_existing(
        &self,
        destination: &Store,
        state: &Path,
        storage_mode: OwnerStorage,
    ) -> io::Result<BindingJournal> {
        let sessions = control(destination, state)?;
        let current = self.bind_existing(destination, state, storage_mode)?;
        destination.with_locked(|directory, snapshot| {
            let intent: Intent = read(directory, INTENT, LIMIT)?;
            validate_intent(&intent, snapshot)?;
            active_journal(directory, &intent)?;
            let mut journal = load(directory, &intent)?;
            journal.switch_requested = true;
            storage::replace_private(directory, Path::new(BINDINGS), &encode(&journal)?)
        })?;
        for (logical, binding) in &current.bindings {
            sessions
                .switch_binding(binding, |binding| {
                    self.binding_guard(destination, binding, storage_mode)
                        .map_err(authorization_error)
                })
                .map_err(control_error)?;
            checkpoint(destination, logical, BindingPhase::Active)?;
        }
        destination.with_locked(|directory, snapshot| {
            let intent: Intent = read(directory, INTENT, LIMIT)?;
            validate_intent(&intent, snapshot)?;
            let mut journal = active_journal(directory, &intent)?;
            journal.phase = Phase::Switched;
            save_journal(directory, &journal)
        })?;
        binding_status(destination)
    }

    pub fn release_bindings(
        &self,
        destination: &Store,
        state: &Path,
    ) -> io::Result<BindingJournal> {
        let current = binding_status(destination)?;
        if current.released.len() == current.bindings.len() {
            return Ok(current);
        }
        let sessions = control(destination, state)?;
        destination.with_locked(|directory, snapshot| {
            if !matches!(
                snapshot.phase,
                HostPhase::Disabled | HostPhase::StopRequested | HostPhase::CleanupPending
            ) {
                return Err(refused("host.migration_disable_before_rollback"));
            }
            let mut journal: Journal = read(directory, JOURNAL, LIMIT)?;
            journal.phase = Phase::RollbackPending;
            save_journal(directory, &journal)
        })?;
        for (logical, binding) in &current.bindings {
            if current.released.contains(logical) {
                continue;
            }
            sessions
                .release_existing(binding, |binding| {
                    destination
                        .with_owned_lease(|directory, snapshot, lease| {
                            check_migration_owner(
                                directory,
                                snapshot,
                                binding.operation.as_str(),
                                &binding.account,
                                true,
                            )?;
                            let intent: Intent = read(directory, INTENT, LIMIT)?;
                            let current = load(directory, &intent)?;
                            if current.bindings.get(binding.logical.as_str()) != Some(binding) {
                                return Err(refused("host.migration_foreign_release"));
                            }
                            Ok(lease)
                        })
                        .map_err(authorization_error)
                })
                .map_err(control_error)?;
            checkpoint(destination, logical, BindingPhase::Released)?;
        }
        binding_status(destination)
    }
}

fn destination_claims(
    sources: &[Loaded],
    intent: &Intent,
    owner: &MigrationOwnerPolicy,
) -> io::Result<()> {
    for source in sources {
        let current: Claim = read(&source.directory, CLAIM, LIMIT)?;
        if current.released
            || current.operation != intent.operation
            || current.destination_installation != owner.installation.installation
            || current.destination_host_id != owner.enrollment.binding.host.host.as_str()
            || current.destination_key_fingerprint
                != owner.enrollment.binding.host.host_key_fingerprint
            || current.source_receipt_digest != digest(&source.retained)?
        {
            return Err(refused("host.migration_source_claim_changed"));
        }
    }
    Ok(())
}

fn checkpoint(destination: &Store, logical: &str, phase: BindingPhase) -> io::Result<()> {
    destination.with_locked(|directory, snapshot| {
        let intent: Intent = read(directory, INTENT, LIMIT)?;
        validate_intent_identity(&intent, snapshot)?;
        let mut current = load(directory, &intent)?;
        if phase != BindingPhase::Released {
            active_journal(directory, &intent)?;
        }
        if !current.bindings.contains_key(logical) {
            return Err(refused("host.migration_unknown_binding_checkpoint"));
        }
        match phase {
            BindingPhase::Prepared => {
                current.bound.insert(logical.into());
            }
            BindingPhase::Active => {
                current.switched.insert(logical.into());
            }
            BindingPhase::Released => {
                current.released.insert(logical.into());
            }
            BindingPhase::Revoked => return Err(refused("host.migration_binding_revoked")),
        }
        storage::replace_private(directory, Path::new(BINDINGS), &encode(&current)?)
    })
}

pub fn binding_status(destination: &Store) -> io::Result<BindingJournal> {
    destination.with_locked(|directory, snapshot| {
        let intent: Intent = read(directory, INTENT, LIMIT)?;
        validate_intent_identity(&intent, snapshot)?;
        load(directory, &intent)
    })
}

pub(super) fn defer_existing(destination: &Store) -> io::Result<Journal> {
    destination.with_locked(|directory, snapshot| {
        let intent: Intent = read(directory, INTENT, LIMIT)?;
        validate_intent(&intent, snapshot)?;
        let binding = load(directory, &intent)?;
        if binding.switch_requested || !binding.released.is_empty() {
            return Err(refused(
                "host.migration_switch_already_requested_use_exact_rollback",
            ));
        }
        let mut journal = active_journal(directory, &intent)?;
        journal.phase = Phase::Deferred;
        save_journal(directory, &journal)?;
        Ok(journal)
    })
}

pub fn has_bindings(destination: &Store) -> io::Result<bool> {
    destination.with_locked(|directory, snapshot| {
        if optional::<BindingJournal>(directory, BINDINGS)?.is_none() {
            return Ok(false);
        }
        let intent: Intent = read(directory, INTENT, LIMIT)?;
        validate_intent_identity(&intent, snapshot)?;
        load(directory, &intent)?;
        Ok(true)
    })
}

pub fn approve_migration_owner(
    destination: &Store,
    operation: &str,
    storage: OwnerStorage,
    public_key: [u8; 32],
    confirmed_fingerprint: &str,
) -> io::Result<crate::enrollment::EnrollmentSnapshot> {
    if crate::enrollment::owner_key_fingerprint(&public_key)? != confirmed_fingerprint {
        return Err(refused("owner.explicit_fingerprint_confirmation_required"));
    }
    let owners = destination.owner_enrollment_with_storage(storage)?;
    let current = owners.snapshot()?;
    let capabilities = festerm_host_protocol::Capabilities {
        inventory: true,
        attach: true,
        detach: true,
        ..Default::default()
    };
    destination.with_locked(|directory, snapshot| {
        check_migration_owner(
            directory,
            snapshot,
            operation,
            &current.binding.account,
            false,
        )
    })?;
    if current.remote_access
        && current.capabilities == capabilities
        && current
            .keys
            .iter()
            .any(|key| key.public_key == public_key && !key.revoked)
    {
        return Ok(current);
    }
    destination.with_locked(|directory, snapshot| {
        check_owner_initialization(directory, snapshot, operation, &current.binding.account)
    })?;
    let enrolled = if current
        .keys
        .iter()
        .any(|key| key.public_key == public_key && !key.revoked)
    {
        current
    } else {
        owners.enroll(current.revision, public_key, confirmed_fingerprint)?
    };
    owners.approve_permissions(enrolled.revision, true, capabilities)
}

pub(crate) fn all_released(directory: &File, intent: &Intent) -> io::Result<()> {
    match optional::<BindingJournal>(directory, BINDINGS)? {
        None => Ok(()),
        Some(_) if load(directory, intent)?.released.len() == intent.plan.sources.len() => Ok(()),
        Some(_) => Err(refused("host.migration_exact_native_release_required")),
    }
}
pub(crate) fn require_switched(directory: &File, intent: &Intent) -> io::Result<()> {
    let journal = load(directory, intent)?;
    if journal.switched.len() != intent.plan.sources.len() || !journal.released.is_empty() {
        return Err(refused("host.migration_native_switch_incomplete"));
    }
    Ok(())
}
