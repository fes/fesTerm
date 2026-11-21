//! Bounded, host-owned transactional storage for JavaScript program saves.
//!
//! This core deliberately does not expose directory handles, paths, UI state,
//! or worker integration. A later program host must bind its opaque IDs to a
//! reviewed installed program and selected universe before constructing it.

use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use festerm_automation_js::{
    program::ProgramLaunchMode, ProgramStorageCommit, ProgramStorageCommitRequest,
    ProgramStorageHost, ProgramStorageLoad, ProgramStoragePreparation,
};
use festerm_sessiond::private_storage::{self, PreparedCheckpoint, UnlockOnDrop};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const STORAGE_VERSION: u8 = 1;
pub(crate) const CURRENT_SAVE_SCHEMA: u32 = 2;
pub(crate) const OLDEST_APPROVED_SAVE_SCHEMA: u32 = 1;
const MAX_PAYLOAD_BYTES: usize = festerm_automation_js::MAX_STORAGE_VALUE_BYTES;
const MAX_ENVELOPE_BYTES: usize = 64 * 1024;
const LOCK_NAME: &str = "writer.lock";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProgramId(Uuid);

impl ProgramId {
    pub(crate) fn new() -> Self {
        Self::from_host(Uuid::new_v4())
    }

    pub(crate) const fn from_host(identity: Uuid) -> Self {
        Self(identity)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct UniverseId(Uuid);

impl UniverseId {
    pub(crate) fn new() -> Self {
        Self::from_host(Uuid::new_v4())
    }

    pub(crate) const fn from_host(identity: Uuid) -> Self {
        Self(identity)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SlotId(Uuid);

impl SlotId {
    pub(crate) fn new() -> Self {
        Self::from_host(Uuid::new_v4())
    }

    pub(crate) const fn from_host(identity: Uuid) -> Self {
        Self(identity)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct OperationId(Uuid);

impl OperationId {
    pub(crate) fn new() -> Self {
        Self::from_host(Uuid::new_v4())
    }

    pub(crate) const fn from_host(identity: Uuid) -> Self {
        Self(identity)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SaveRevision(u64);

impl SaveRevision {
    pub(crate) const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SaveSnapshot {
    pub(crate) revision: SaveRevision,
    pub(crate) schema_version: u32,
    pub(crate) value: Vec<u8>,
    operation_id: OperationId,
}

impl SaveSnapshot {
    pub(crate) fn operation_id(&self) -> OperationId {
        self.operation_id
    }
}

const DEVELOPER_PROGRAM_ID: Uuid = Uuid::from_u128(0x6d3ac8f4_67f4_49d0_b10a_bf7e8f10d188);
const DEVELOPER_UNIVERSE_ID: Uuid = Uuid::from_u128(0x8d3ad0e2_62f1_47f7_9db5_0fdf8bbd18c4);
const DEVELOPER_SLOT_ID: Uuid = Uuid::from_u128(0x1e8fbd1d_98d3_4d58_a0a0_d4d0a5c2e6b7);
const DEVELOPER_SLOT: &str = "primary";
const GENERIC_UNIVERSE_SLOT: &str = "universe";

pub(crate) fn application_data_root() -> Option<PathBuf> {
    std::env::var_os("FESTERM_JAVASCRIPT_PROGRAM_DATA_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            directories::ProjectDirs::from("com", "fes", "fesTerm")
                .map(|project| project.data_dir().to_path_buf())
        })
}

fn identity_from_digest(label: &str, digest: &str) -> Uuid {
    let mut hasher = Sha256::new();
    hasher.update(label.as_bytes());
    hasher.update([0]);
    hasher.update(digest.as_bytes());
    let bytes: [u8; 32] = hasher.finalize().into();
    Uuid::from_bytes(
        bytes[..16]
            .try_into()
            .expect("SHA-256 has at least 16 bytes"),
    )
}

fn program_ids(digest: &str) -> (ProgramId, UniverseId, SlotId) {
    (
        ProgramId::from_host(identity_from_digest("program", digest)),
        UniverseId::from_host(identity_from_digest("universe", digest)),
        SlotId::from_host(identity_from_digest("slot", digest)),
    )
}

pub(crate) fn program_storage_path(root: &Path, digest: &str) -> PathBuf {
    let (program, universe, _) = program_ids(digest);
    root.join("javascript-programs")
        .join(format!("program-{}", program.0.simple()))
        .join(format!("universe-{}", universe.0.simple()))
}

pub(crate) fn program_storage_bytes(root: &Path, digest: &str) -> io::Result<u64> {
    let directory = program_storage_path(root, digest);
    let (_, _, slot) = program_ids(digest);
    let prefix = format!("save-{}", slot.0.simple());
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut bytes = 0u64;
    for entry in entries.take(64) {
        let entry = entry?;
        if entry.file_type()?.is_file() && entry.file_name().to_string_lossy().starts_with(&prefix)
        {
            bytes = bytes.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(bytes)
}

pub(crate) fn delete_program_storage(root: &Path, digest: &str) -> io::Result<bool> {
    let (program, universe, slot) = program_ids(digest);
    let mut store = match TransactionalSaveStore::open(root, program, universe, slot) {
        Ok(store) => store,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let removed = store.delete_payload_files()?;
    drop(store);
    Ok(removed)
}

pub(crate) struct ReviewedDeveloperStorageHost {
    root: PathBuf,
    mode: ProgramLaunchMode,
    store: Mutex<Option<TransactionalSaveStore>>,
    program_ids: (ProgramId, UniverseId, SlotId),
    slot: String,
    max_bytes: usize,
    require_json: bool,
}

impl ReviewedDeveloperStorageHost {
    pub(crate) fn from_application_data(root: PathBuf, mode: ProgramLaunchMode) -> Self {
        Self {
            root,
            mode,
            store: Mutex::new(None),
            program_ids: (
                ProgramId::from_host(DEVELOPER_PROGRAM_ID),
                UniverseId::from_host(DEVELOPER_UNIVERSE_ID),
                SlotId::from_host(DEVELOPER_SLOT_ID),
            ),
            slot: DEVELOPER_SLOT.into(),
            max_bytes: MAX_PAYLOAD_BYTES,
            require_json: false,
        }
    }

    pub(crate) fn for_program(
        root: PathBuf,
        digest: &str,
        mode: ProgramLaunchMode,
        max_bytes: usize,
    ) -> Self {
        Self {
            root,
            mode,
            store: Mutex::new(None),
            program_ids: program_ids(digest),
            slot: GENERIC_UNIVERSE_SLOT.into(),
            max_bytes: max_bytes.min(MAX_PAYLOAD_BYTES),
            require_json: true,
        }
    }

    #[cfg(test)]
    fn from_test(store: TransactionalSaveStore, mode: ProgramLaunchMode) -> Self {
        Self {
            root: PathBuf::new(),
            mode,
            store: Mutex::new(Some(store)),
            program_ids: (
                ProgramId::from_host(DEVELOPER_PROGRAM_ID),
                UniverseId::from_host(DEVELOPER_UNIVERSE_ID),
                SlotId::from_host(DEVELOPER_SLOT_ID),
            ),
            slot: DEVELOPER_SLOT.into(),
            max_bytes: MAX_PAYLOAD_BYTES,
            require_json: false,
        }
    }

    #[cfg(test)]
    fn from_test_program(
        store: TransactionalSaveStore,
        mode: ProgramLaunchMode,
        ids: (ProgramId, UniverseId, SlotId),
        slot: &str,
        max_bytes: usize,
    ) -> Self {
        Self {
            root: PathBuf::new(),
            mode,
            store: Mutex::new(Some(store)),
            program_ids: ids,
            slot: slot.into(),
            max_bytes,
            require_json: true,
        }
    }

    fn with_store<T>(
        &self,
        operation: impl FnOnce(&mut TransactionalSaveStore) -> T,
    ) -> Result<T, ProgramStoragePreparation> {
        let mut guard = self
            .store
            .lock()
            .map_err(|_| ProgramStoragePreparation::Unavailable)?;
        if guard.is_none() {
            let store = TransactionalSaveStore::open(
                &self.root,
                self.program_ids.0,
                self.program_ids.1,
                self.program_ids.2,
            )
            .map_err(|error| {
                if error.kind() == io::ErrorKind::WouldBlock {
                    ProgramStoragePreparation::BusyWriter
                } else {
                    ProgramStoragePreparation::Unavailable
                }
            })?;
            *guard = Some(store);
        }
        Ok(operation(
            guard
                .as_mut()
                .expect("opened JavaScript program storage is retained"),
        ))
    }

    fn parse_operation_id(&self, operation_id: &str) -> Option<OperationId> {
        let parsed = Uuid::parse_str(operation_id).ok()?;
        (parsed.hyphenated().to_string() == operation_id).then_some(OperationId::from_host(parsed))
    }
}

impl ProgramStorageHost for ReviewedDeveloperStorageHost {
    fn prepare(&self) -> Result<(), ProgramStoragePreparation> {
        let loaded = self.with_store(|store| {
            let loaded = store.load();
            if self.mode == ProgramLaunchMode::Resume {
                if let SaveLoad::Present(snapshot) = &loaded {
                    if store.has_uncertain_publication() {
                        let reconciliation = store.resolve_uncertain();
                        if !matches!(reconciliation, SaveCommit::Committed(_)) {
                            return match reconciliation {
                                SaveCommit::Corrupt(_) => SaveLoad::Corrupt(
                                    "the prior save could not be reconciled".to_owned(),
                                ),
                                SaveCommit::Unavailable { kind, message } => {
                                    SaveLoad::Unavailable { kind, message }
                                }
                                SaveCommit::Uncertain { message, .. } => SaveLoad::Unavailable {
                                    kind: io::ErrorKind::Other,
                                    message,
                                },
                                SaveCommit::Conflict { message, .. } => SaveLoad::Unavailable {
                                    kind: io::ErrorKind::Other,
                                    message,
                                },
                                SaveCommit::Future {
                                    actual_schema_version,
                                } => SaveLoad::Unavailable {
                                    kind: io::ErrorKind::InvalidData,
                                    message: format!(
                                        "the prior save uses newer schema {actual_schema_version}"
                                    ),
                                },
                                SaveCommit::Committed(_) => unreachable!(),
                            };
                        }
                    }
                    let _ = snapshot;
                }
            }
            loaded
        })?;
        match loaded {
            SaveLoad::Missing if self.mode == ProgramLaunchMode::New => Ok(()),
            SaveLoad::Missing => Err(ProgramStoragePreparation::MissingSave),
            SaveLoad::Present(snapshot) if snapshot.schema_version > CURRENT_SAVE_SCHEMA => {
                Err(ProgramStoragePreparation::Future)
            }
            SaveLoad::Present(snapshot)
                if snapshot.schema_version < OLDEST_APPROVED_SAVE_SCHEMA =>
            {
                Err(ProgramStoragePreparation::UnapprovedSchema)
            }
            SaveLoad::Present(snapshot) if self.mode == ProgramLaunchMode::New => {
                let _ = snapshot;
                Err(ProgramStoragePreparation::ExistingSave)
            }
            SaveLoad::Present(_) => Ok(()),
            SaveLoad::Corrupt(_) => Err(ProgramStoragePreparation::Corrupt),
            SaveLoad::Unavailable {
                kind: io::ErrorKind::WouldBlock,
                ..
            } => Err(ProgramStoragePreparation::BusyWriter),
            SaveLoad::Unavailable { .. } => Err(ProgramStoragePreparation::Unavailable),
        }
    }

    fn load(&self, slot: &str) -> ProgramStorageLoad {
        if slot != self.slot {
            return ProgramStorageLoad::Unavailable;
        }
        match self.with_store(|store| store.load()) {
            Ok(SaveLoad::Missing) => ProgramStorageLoad::Missing,
            Ok(SaveLoad::Present(snapshot)) if snapshot.schema_version > CURRENT_SAVE_SCHEMA => {
                ProgramStorageLoad::Future {
                    actual_schema_version: snapshot.schema_version,
                }
            }
            Ok(SaveLoad::Present(snapshot))
                if snapshot.schema_version < OLDEST_APPROVED_SAVE_SCHEMA =>
            {
                ProgramStorageLoad::Corrupt
            }
            Ok(SaveLoad::Present(snapshot)) => ProgramStorageLoad::Present {
                schema_version: snapshot.schema_version,
                revision: snapshot.revision.value(),
                value: snapshot.value,
            },
            Ok(SaveLoad::Corrupt(_)) | Err(ProgramStoragePreparation::Corrupt) => {
                ProgramStorageLoad::Corrupt
            }
            Err(ProgramStoragePreparation::BusyWriter) => ProgramStorageLoad::BusyWriter,
            Err(ProgramStoragePreparation::UnapprovedSchema) => ProgramStorageLoad::Corrupt,
            Err(_) => ProgramStorageLoad::Unavailable,
            Ok(SaveLoad::Unavailable {
                kind: io::ErrorKind::WouldBlock,
                ..
            }) => ProgramStorageLoad::BusyWriter,
            Ok(SaveLoad::Unavailable { .. }) => ProgramStorageLoad::Unavailable,
        }
    }

    fn commit(&self, slot: &str, request: ProgramStorageCommitRequest) -> ProgramStorageCommit {
        if slot != self.slot
            || request.schema_version != CURRENT_SAVE_SCHEMA
            || request.value.len() > self.max_bytes
            || (self.require_json
                && serde_json::from_slice::<serde_json::Value>(&request.value).is_err())
        {
            return ProgramStorageCommit::ResourceLimit;
        }
        let Some(operation_id) = self.parse_operation_id(&request.operation_id) else {
            return ProgramStorageCommit::ResourceLimit;
        };
        let expected_revision = request.expected_revision.map(SaveRevision);
        let result = self.with_store(|store| {
            let migrate = match store.load() {
                SaveLoad::Present(snapshot) if snapshot.schema_version > CURRENT_SAVE_SCHEMA => {
                    return SaveCommit::Future {
                        actual_schema_version: snapshot.schema_version,
                    };
                }
                SaveLoad::Present(snapshot) => snapshot.schema_version < request.schema_version,
                SaveLoad::Missing => false,
                _ => false,
            };
            if migrate {
                if self.mode != ProgramLaunchMode::Resume
                    || request.schema_version != CURRENT_SAVE_SCHEMA
                {
                    return SaveCommit::Conflict {
                        actual_revision: expected_revision,
                        message: "migration is refused outside explicit resume".to_owned(),
                    };
                }
                store.commit_migrating(
                    expected_revision,
                    operation_id,
                    request.schema_version,
                    &request.value,
                )
            } else {
                store.commit(expected_revision, operation_id, &request.value)
            }
        });
        match result {
            Ok(SaveCommit::Committed(receipt)) => ProgramStorageCommit::Committed {
                revision: receipt.revision.value(),
            },
            Ok(SaveCommit::Conflict {
                actual_revision, ..
            }) => ProgramStorageCommit::Conflict {
                actual_revision: actual_revision.map(SaveRevision::value),
            },
            Ok(SaveCommit::Corrupt(_)) | Err(ProgramStoragePreparation::Corrupt) => {
                ProgramStorageCommit::Corrupt
            }
            Ok(SaveCommit::Future {
                actual_schema_version,
            }) => ProgramStorageCommit::Future {
                actual_schema_version,
            },
            Ok(SaveCommit::Uncertain { .. }) => ProgramStorageCommit::Uncertain,
            Ok(SaveCommit::Unavailable {
                kind: io::ErrorKind::WouldBlock,
                ..
            })
            | Err(ProgramStoragePreparation::BusyWriter) => ProgramStorageCommit::BusyWriter,
            Err(_) | Ok(SaveCommit::Unavailable { .. }) => ProgramStorageCommit::Unavailable,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct SaveReceipt {
    pub(crate) revision: SaveRevision,
    pub(crate) operation_id: OperationId,
    pub(crate) payload_digest: [u8; 32],
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum SaveLoad {
    Missing,
    Present(SaveSnapshot),
    Corrupt(String),
    Unavailable {
        kind: io::ErrorKind,
        message: String,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum SaveCommit {
    Committed(SaveReceipt),
    Future {
        actual_schema_version: u32,
    },
    Conflict {
        actual_revision: Option<SaveRevision>,
        message: String,
    },
    Corrupt(String),
    Uncertain {
        operation_id: OperationId,
        message: String,
    },
    Unavailable {
        kind: io::ErrorKind,
        message: String,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SaveEnvelope {
    version: u8,
    program_id: String,
    universe_id: String,
    slot_id: String,
    revision: u64,
    schema_version: u32,
    operation_id: String,
    payload_digest: [u8; 32],
    payload: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct UncertainCommit {
    operation_id: OperationId,
    payload_digest: [u8; 32],
}

pub(crate) struct TransactionalSaveStore {
    program_id: ProgramId,
    universe_id: UniverseId,
    slot_id: SlotId,
    directory: File,
    writer: UnlockOnDrop,
    uncertain: Option<UncertainCommit>,
}

impl TransactionalSaveStore {
    pub(crate) fn open(
        root: &Path,
        program_id: ProgramId,
        universe_id: UniverseId,
        slot_id: SlotId,
    ) -> io::Result<Self> {
        let root_directory = private_storage::private_directory(root, false)?;
        let namespace_path = root.join("javascript-programs");
        let namespace = private_storage::private_directory(&namespace_path, true)?;
        drop(root_directory);
        let program_path = namespace_path.join(format!("program-{}", program_id.0.simple()));
        let program = private_storage::private_directory(&program_path, true)?;
        drop(namespace);
        let universe_path = program_path.join(format!("universe-{}", universe_id.0.simple()));
        let directory = private_storage::private_directory(&universe_path, true)?;
        drop(program);
        Self::open_directory(directory, program_id, universe_id, slot_id)
    }

    #[cfg(test)]
    fn open_test(
        directory: File,
        program_id: ProgramId,
        universe_id: UniverseId,
        slot_id: SlotId,
    ) -> io::Result<Self> {
        Self::open_directory(directory, program_id, universe_id, slot_id)
    }

    fn open_directory(
        directory: File,
        program_id: ProgramId,
        universe_id: UniverseId,
        slot_id: SlotId,
    ) -> io::Result<Self> {
        let writer = private_storage::open_trusted_lock(&directory, Path::new(LOCK_NAME))?;
        fs2::FileExt::try_lock_exclusive(&writer).map_err(|error| {
            if error.kind() == io::ErrorKind::WouldBlock {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "JavaScript program universe already has a writer",
                )
            } else {
                error
            }
        })?;
        let writer = UnlockOnDrop(writer);
        let mut store = Self {
            program_id,
            universe_id,
            slot_id,
            directory,
            writer,
            uncertain: None,
        };
        store.verify_writer()?;
        // A new process cannot recover the previous acknowledgement. Fence the
        // latest envelope until that exact operation is verified and synced.
        if let Ok(Some(envelope)) = store.read_envelope() {
            store.uncertain = Some(UncertainCommit {
                operation_id: OperationId(
                    Uuid::parse_str(&envelope.operation_id).expect("validated operation ID"),
                ),
                payload_digest: envelope.payload_digest,
            });
        }
        Ok(store)
    }

    pub(crate) fn load(&self) -> SaveLoad {
        match self.verify_writer() {
            Ok(()) => self.read_current(),
            Err(error) => SaveLoad::Unavailable {
                kind: error.kind(),
                message: error.to_string(),
            },
        }
    }

    pub(crate) fn has_uncertain_publication(&self) -> bool {
        self.uncertain.is_some()
    }

    fn delete_payload_files(&mut self) -> io::Result<bool> {
        self.verify_writer()?;
        let raw =
            private_storage::read_private(&self.directory, &self.save_name(), MAX_ENVELOPE_BYTES);
        let existed = match raw {
            Ok(ref bytes) => {
                if let Ok(envelope) = serde_json::from_slice::<SaveEnvelope>(bytes) {
                    Some(envelope.revision)
                } else {
                    Some(0)
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(_) => Some(0),
        };
        private_storage::remove_private_file(&self.directory, &self.save_name())?;
        if let Some(revision) = existed.and_then(|revision| revision.checked_sub(1)) {
            private_storage::remove_private_file(
                &self.directory,
                &self.migration_backup_name(revision),
            )?;
        }
        self.uncertain = None;
        if existed.is_some() {
            sync_publication_directory(&self.directory)?;
        }
        Ok(existed.is_some())
    }

    pub(crate) fn commit(
        &mut self,
        expected_revision: Option<SaveRevision>,
        operation_id: OperationId,
        payload: &[u8],
    ) -> SaveCommit {
        self.commit_with_schema(
            expected_revision,
            operation_id,
            CURRENT_SAVE_SCHEMA,
            payload,
            false,
            None,
        )
    }

    pub(crate) fn commit_migrating(
        &mut self,
        expected_revision: Option<SaveRevision>,
        operation_id: OperationId,
        schema_version: u32,
        payload: &[u8],
    ) -> SaveCommit {
        self.commit_with_schema(
            expected_revision,
            operation_id,
            schema_version,
            payload,
            true,
            None,
        )
    }

    fn commit_with_fault(
        &mut self,
        expected_revision: Option<SaveRevision>,
        operation_id: OperationId,
        payload: &[u8],
        fault: Option<CommitFault>,
    ) -> SaveCommit {
        self.commit_with_schema(
            expected_revision,
            operation_id,
            CURRENT_SAVE_SCHEMA,
            payload,
            false,
            fault,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_with_schema(
        &mut self,
        expected_revision: Option<SaveRevision>,
        operation_id: OperationId,
        schema_version: u32,
        payload: &[u8],
        migrate: bool,
        fault: Option<CommitFault>,
    ) -> SaveCommit {
        if schema_version == 0 || schema_version > CURRENT_SAVE_SCHEMA {
            return SaveCommit::Corrupt(
                "JavaScript program save schema version is not approved".to_owned(),
            );
        }
        if payload.len() > MAX_PAYLOAD_BYTES {
            return SaveCommit::Unavailable {
                kind: io::ErrorKind::InvalidInput,
                message: format!(
                    "JavaScript program save payload exceeds the {MAX_PAYLOAD_BYTES}-byte limit"
                ),
            };
        }
        if let Err(error) = self.verify_writer() {
            return unavailable(error);
        }
        let payload_digest: [u8; 32] = Sha256::digest(payload).into();
        if let Some(uncertain) = self.uncertain.clone() {
            if uncertain.operation_id == operation_id && uncertain.payload_digest == payload_digest
            {
                return self.resolve_uncertain();
            }
            return SaveCommit::Uncertain {
                operation_id: uncertain.operation_id,
                message: "a previous JavaScript program save still has an uncertain outcome"
                    .to_owned(),
            };
        }

        let envelope = match self.read_envelope() {
            Ok(envelope) => envelope,
            Err(ReadEnvelope::Missing) => None,
            Err(ReadEnvelope::Corrupt(message)) => return SaveCommit::Corrupt(message),
            Err(ReadEnvelope::Unavailable(error)) => return unavailable(error),
        };
        let actual_revision = envelope
            .as_ref()
            .map(|stored| SaveRevision(stored.revision));
        if let Some(stored) = &envelope {
            if schema_version < stored.schema_version
                || (migrate && schema_version <= stored.schema_version)
                || (!migrate && schema_version != stored.schema_version)
            {
                return SaveCommit::Conflict {
                    actual_revision,
                    message: "the save schema cannot be changed by this commit".to_owned(),
                };
            }
            if Uuid::parse_str(&stored.operation_id).ok() == Some(operation_id.0)
                && stored.payload_digest == payload_digest
            {
                return SaveCommit::Committed(SaveReceipt {
                    revision: SaveRevision(stored.revision),
                    operation_id,
                    payload_digest,
                });
            }
            if Uuid::parse_str(&stored.operation_id).ok() == Some(operation_id.0) {
                return SaveCommit::Conflict {
                    actual_revision,
                    message: "the operation ID already identifies different save data".to_owned(),
                };
            }
        }
        if actual_revision != expected_revision {
            return SaveCommit::Conflict {
                actual_revision,
                message: "the save revision changed before this commit".to_owned(),
            };
        }
        let revision = match actual_revision {
            Some(revision) => {
                let Some(next) = revision.0.checked_add(1) else {
                    return SaveCommit::Corrupt(
                        "JavaScript program save revision cannot be advanced".to_owned(),
                    );
                };
                next
            }
            None => 1,
        };
        let bytes = match serde_json::to_vec(&SaveEnvelope {
            version: STORAGE_VERSION,
            program_id: self.program_id.0.simple().to_string(),
            universe_id: self.universe_id.0.simple().to_string(),
            slot_id: self.slot_id.0.simple().to_string(),
            revision,
            schema_version,
            operation_id: operation_id.0.simple().to_string(),
            payload_digest,
            payload: payload.to_vec(),
        }) {
            Ok(bytes) if bytes.len() <= MAX_ENVELOPE_BYTES => bytes,
            Ok(_) => {
                return SaveCommit::Unavailable {
                    kind: io::ErrorKind::InvalidInput,
                    message: format!(
                        "JavaScript program save envelope exceeds the {MAX_ENVELOPE_BYTES}-byte limit"
                    ),
                };
            }
            Err(error) => {
                return SaveCommit::Unavailable {
                    kind: io::ErrorKind::InvalidData,
                    message: error.to_string(),
                };
            }
        };

        let mut prepared =
            match PreparedCheckpoint::prepare(&self.directory, &self.save_name(), &bytes) {
                Ok(prepared) => prepared,
                Err(error) => return unavailable(error),
            };
        if migrate {
            let Some(stored) = envelope.as_ref() else {
                return SaveCommit::Corrupt(
                    "a save migration requires a prior envelope".to_owned(),
                );
            };
            let prior = match private_storage::read_private(
                &self.directory,
                &self.save_name(),
                MAX_ENVELOPE_BYTES,
            ) {
                Ok(prior) => prior,
                Err(error) => return unavailable(error),
            };
            let mut backup = match PreparedCheckpoint::prepare(
                &self.directory,
                &self.migration_backup_name(stored.revision),
                &prior,
            ) {
                Ok(backup) => backup,
                Err(error) => return unavailable(error),
            };
            if let Err(error) = backup.publish().and_then(|()| backup.sync()) {
                return unavailable(error);
            }
            if let Err(error) = sync_publication_directory(&self.directory) {
                return unavailable(error);
            }
        }
        if fault == Some(CommitFault::BeforePublish) {
            return SaveCommit::Unavailable {
                kind: io::ErrorKind::Other,
                message: "injected JavaScript program save failure before publication".to_owned(),
            };
        }
        if let Err(error) = self.verify_writer() {
            return unavailable(error);
        }
        if let Err(error) = prepared.publish() {
            return unavailable(error);
        }
        self.uncertain = Some(UncertainCommit {
            operation_id,
            payload_digest,
        });
        if fault == Some(CommitFault::AfterPublish) {
            return SaveCommit::Uncertain {
                operation_id,
                message: "injected JavaScript program save failure after publication".to_owned(),
            };
        }
        match self.verify_published(operation_id, payload_digest) {
            Ok(receipt) => match prepared.sync() {
                Ok(()) => match sync_publication_directory(&self.directory) {
                    Ok(()) => {
                        self.uncertain = None;
                        SaveCommit::Committed(receipt)
                    }
                    Err(error) => SaveCommit::Uncertain {
                        operation_id,
                        message: format!("published save could not be synchronized: {error}"),
                    },
                },
                Err(error) => SaveCommit::Uncertain {
                    operation_id,
                    message: format!("published save could not be synchronized: {error}"),
                },
            },
            Err(message) => SaveCommit::Uncertain {
                operation_id,
                message,
            },
        }
    }

    fn resolve_uncertain(&mut self) -> SaveCommit {
        let Some(uncertain) = self.uncertain.clone() else {
            return SaveCommit::Uncertain {
                operation_id: OperationId(Uuid::nil()),
                message: "no uncertain JavaScript program save exists".to_owned(),
            };
        };
        match self.verify_published(uncertain.operation_id, uncertain.payload_digest) {
            Ok(receipt) => match sync_publication_directory(&self.directory) {
                Ok(()) => {
                    self.uncertain = None;
                    SaveCommit::Committed(receipt)
                }
                Err(error) => SaveCommit::Uncertain {
                    operation_id: uncertain.operation_id,
                    message: format!("published save could not be synchronized: {error}"),
                },
            },
            Err(message) => SaveCommit::Uncertain {
                operation_id: uncertain.operation_id,
                message,
            },
        }
    }

    fn verify_published(
        &self,
        operation_id: OperationId,
        payload_digest: [u8; 32],
    ) -> Result<SaveReceipt, String> {
        let envelope = match self.read_envelope() {
            Ok(Some(envelope)) => envelope,
            Ok(None) => {
                return Err("the published JavaScript program save is missing".to_owned());
            }
            Err(ReadEnvelope::Corrupt(message)) => return Err(message),
            Err(ReadEnvelope::Unavailable(error)) => return Err(error.to_string()),
            Err(ReadEnvelope::Missing) => {
                return Err("the published JavaScript program save is missing".to_owned());
            }
        };
        if Uuid::parse_str(&envelope.operation_id).ok() != Some(operation_id.0)
            || envelope.payload_digest != payload_digest
        {
            return Err(
                "the published JavaScript program save does not match the operation".to_owned(),
            );
        }
        Ok(SaveReceipt {
            revision: SaveRevision(envelope.revision),
            operation_id,
            payload_digest,
        })
    }

    fn read_current(&self) -> SaveLoad {
        match self.read_envelope() {
            Ok(Some(envelope)) => SaveLoad::Present(SaveSnapshot {
                revision: SaveRevision(envelope.revision),
                schema_version: envelope.schema_version,
                value: envelope.payload,
                operation_id: OperationId(
                    Uuid::parse_str(&envelope.operation_id).expect("validated operation ID"),
                ),
            }),
            Ok(None) | Err(ReadEnvelope::Missing) => SaveLoad::Missing,
            Err(ReadEnvelope::Corrupt(message)) => SaveLoad::Corrupt(message),
            Err(ReadEnvelope::Unavailable(error)) => SaveLoad::Unavailable {
                kind: error.kind(),
                message: error.to_string(),
            },
        }
    }

    fn read_envelope(&self) -> Result<Option<SaveEnvelope>, ReadEnvelope> {
        let bytes =
            private_storage::read_private(&self.directory, &self.save_name(), MAX_ENVELOPE_BYTES)
                .map_err(|error| {
                if error.kind() == io::ErrorKind::NotFound {
                    ReadEnvelope::Missing
                } else if error.kind() == io::ErrorKind::InvalidInput
                    || error.kind() == io::ErrorKind::InvalidData
                {
                    ReadEnvelope::Corrupt(format!(
                        "JavaScript program save exceeds its bounded envelope: {error}"
                    ))
                } else {
                    ReadEnvelope::Unavailable(error)
                }
            })?;
        let envelope: SaveEnvelope = serde_json::from_slice(&bytes).map_err(|error| {
            ReadEnvelope::Corrupt(format!("JavaScript program save is invalid: {error}"))
        })?;
        if envelope.version != STORAGE_VERSION {
            return Err(ReadEnvelope::Corrupt(
                "JavaScript program save uses an unsupported version".to_owned(),
            ));
        }
        if envelope.program_id != self.program_id.0.simple().to_string()
            || envelope.universe_id != self.universe_id.0.simple().to_string()
            || envelope.slot_id != self.slot_id.0.simple().to_string()
        {
            return Err(ReadEnvelope::Corrupt(
                "JavaScript program save belongs to another storage namespace".to_owned(),
            ));
        }
        let operation_id = Uuid::parse_str(&envelope.operation_id).map_err(|_| {
            ReadEnvelope::Corrupt("JavaScript program save operation ID is invalid".to_owned())
        })?;
        if operation_id.as_simple().to_string() != envelope.operation_id {
            return Err(ReadEnvelope::Corrupt(
                "JavaScript program save operation ID is not canonical".to_owned(),
            ));
        }
        if envelope.revision == 0
            || envelope.revision > festerm_automation_js::protocol::MAX_SAFE_INTEGER
            || envelope.schema_version == 0
            || Sha256::digest(&envelope.payload).as_slice() != envelope.payload_digest
            || envelope.payload.len() > MAX_PAYLOAD_BYTES
        {
            return Err(ReadEnvelope::Corrupt(
                "JavaScript program save payload or revision is invalid".to_owned(),
            ));
        }
        Ok(Some(envelope))
    }

    fn save_name(&self) -> PathBuf {
        PathBuf::from(format!("save-{}.json", self.slot_id.0.simple()))
    }

    fn migration_backup_name(&self, revision: u64) -> PathBuf {
        PathBuf::from(format!(
            "save-{}-pre-migration-r{revision}.json",
            self.slot_id.0.simple()
        ))
    }

    fn verify_writer(&self) -> io::Result<()> {
        let current = private_storage::open_trusted_lock(&self.directory, Path::new(LOCK_NAME))?;
        if !same_file(&self.writer, &current)? {
            return Err(io::Error::other(
                "JavaScript program universe writer lock changed",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommitFault {
    BeforePublish,
    AfterPublish,
}

enum ReadEnvelope {
    Missing,
    Corrupt(String),
    Unavailable(io::Error),
}

fn unavailable(error: io::Error) -> SaveCommit {
    SaveCommit::Unavailable {
        kind: error.kind(),
        message: error.to_string(),
    }
}

#[cfg(unix)]
fn same_file(left: &File, right: &File) -> io::Result<bool> {
    let left = nix::sys::stat::fstat(left).map_err(io::Error::from)?;
    let right = nix::sys::stat::fstat(right).map_err(io::Error::from)?;
    Ok(left.st_dev == right.st_dev && left.st_ino == right.st_ino)
}

#[cfg(windows)]
fn same_file(left: &File, right: &File) -> io::Result<bool> {
    festerm_windows_security::same_file_identity(left, right)
}

#[cfg(unix)]
fn sync_publication_directory(directory: &File) -> io::Result<()> {
    directory.sync_all()
}

#[cfg(windows)]
fn sync_publication_directory(directory: &File) -> io::Result<()> {
    festerm_windows_security::lock_directory_for_sync(directory)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PrivateFixture(PathBuf);

    impl PrivateFixture {
        fn new() -> Self {
            let name = format!(".javascript-save-test-{}", Uuid::new_v4().simple());
            let path = std::env::current_dir().unwrap().join(&name);
            #[cfg(unix)]
            let parent = File::open(path.parent().unwrap()).unwrap();
            #[cfg(windows)]
            let parent = festerm_windows_security::lock_directory_path_without_delete_sharing(
                path.parent().unwrap(),
            )
            .unwrap();
            #[cfg(unix)]
            drop(
                festerm_unix_security::create_private_directory(&parent, Path::new(&name)).unwrap(),
            );
            #[cfg(windows)]
            drop(
                festerm_windows_security::create_current_user_only_directory(
                    &parent,
                    Path::new(&name),
                )
                .unwrap(),
            );
            Self(path)
        }

        fn directory(&self) -> File {
            #[cfg(unix)]
            {
                File::open(&self.0).unwrap()
            }
            #[cfg(windows)]
            {
                festerm_windows_security::lock_directory_path_without_delete_sharing(&self.0)
                    .unwrap()
            }
        }
    }

    impl Drop for PrivateFixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn digest_bound_program_storage_honors_quota_and_delete() {
        let root = PrivateFixture::new();
        let digest = "ab".repeat(32);
        let ids = program_ids(&digest);
        let host = ReviewedDeveloperStorageHost::from_test_program(
            TransactionalSaveStore::open_test(root.directory(), ids.0, ids.1, ids.2).unwrap(),
            ProgramLaunchMode::New,
            ids,
            GENERIC_UNIVERSE_SLOT,
            4096,
        );
        assert_eq!(host.prepare(), Ok(()));
        assert!(matches!(
            host.commit(
                GENERIC_UNIVERSE_SLOT,
                ProgramStorageCommitRequest {
                    expected_revision: None,
                    schema_version: CURRENT_SAVE_SCHEMA,
                    value: vec![1; 4097],
                    operation_id: OperationId::new().0.hyphenated().to_string(),
                },
            ),
            ProgramStorageCommit::ResourceLimit
        ));
        assert!(matches!(
            host.commit(
                GENERIC_UNIVERSE_SLOT,
                ProgramStorageCommitRequest {
                    expected_revision: None,
                    schema_version: CURRENT_SAVE_SCHEMA,
                    value: b"not-json".to_vec(),
                    operation_id: OperationId::new().0.hyphenated().to_string(),
                },
            ),
            ProgramStorageCommit::ResourceLimit
        ));
        assert!(matches!(
            host.commit(
                GENERIC_UNIVERSE_SLOT,
                ProgramStorageCommitRequest {
                    expected_revision: None,
                    schema_version: CURRENT_SAVE_SCHEMA,
                    value: br#"{"sector":2}"#.to_vec(),
                    operation_id: OperationId::new().0.hyphenated().to_string(),
                },
            ),
            ProgramStorageCommit::Committed { revision: 1 }
        ));
        drop(host);
        let mut store =
            TransactionalSaveStore::open_test(root.directory(), ids.0, ids.1, ids.2).unwrap();
        assert!(store.delete_payload_files().unwrap());
        assert!(matches!(store.load(), SaveLoad::Missing));
    }

    fn fixture() -> (PrivateFixture, ProgramId, UniverseId, SlotId) {
        (
            PrivateFixture::new(),
            ProgramId::new(),
            UniverseId::new(),
            SlotId::new(),
        )
    }

    #[test]
    fn javascript_program_commit_is_atomic_revision_checked_and_recoverable() {
        let (root, program, universe, slot) = fixture();
        let mut store =
            TransactionalSaveStore::open_test(root.directory(), program, universe, slot).unwrap();
        assert_eq!(store.load(), SaveLoad::Missing);

        let first_operation = OperationId::new();
        let SaveCommit::Committed(first) = store.commit(None, first_operation, b"first") else {
            panic!("initial commit failed");
        };
        assert_eq!(first.revision.value(), 1);
        assert_eq!(
            store.commit(None, OperationId::new(), b"wrong"),
            SaveCommit::Conflict {
                actual_revision: Some(first.revision),
                message: "the save revision changed before this commit".to_owned(),
            }
        );
        assert_eq!(
            store.commit(Some(first.revision), first_operation, b"different"),
            SaveCommit::Conflict {
                actual_revision: Some(first.revision),
                message: "the operation ID already identifies different save data".to_owned(),
            }
        );
        let second_operation = OperationId::new();
        let SaveCommit::Committed(second) =
            store.commit(Some(first.revision), second_operation, b"second")
        else {
            panic!("revision-checked commit failed");
        };
        assert_eq!(second.revision.value(), 2);
        assert_eq!(
            store.commit(None, second_operation, b"second"),
            SaveCommit::Committed(SaveReceipt {
                revision: second.revision,
                operation_id: second_operation,
                payload_digest: Sha256::digest(b"second").into(),
            })
        );
        let SaveLoad::Present(saved) = store.load() else {
            panic!("committed save did not reload");
        };
        assert_eq!(saved.revision, second.revision);
        assert_eq!(saved.value, b"second");
        assert_eq!(saved.operation_id(), second_operation);
        drop(store);

        let mut recovered =
            TransactionalSaveStore::open_test(root.directory(), program, universe, slot).unwrap();
        assert!(matches!(
            recovered.commit(Some(second.revision), OperationId::new(), b"next"),
            SaveCommit::Uncertain {
                operation_id,
                ..
            } if operation_id == second_operation
        ));
        assert!(matches!(
            recovered.commit(Some(second.revision), second_operation, b"second"),
            SaveCommit::Committed(receipt) if receipt.revision == second.revision
        ));
    }

    #[test]
    fn javascript_program_save_faults_preserve_or_report_real_publication() {
        let (root, program, universe, slot) = fixture();
        let mut store =
            TransactionalSaveStore::open_test(root.directory(), program, universe, slot).unwrap();
        let first_operation = OperationId::new();
        let SaveCommit::Committed(first) = store.commit(None, first_operation, b"old") else {
            panic!("initial commit failed");
        };

        assert!(matches!(
            store.commit_with_fault(
                Some(first.revision),
                OperationId::new(),
                b"not-published",
                Some(CommitFault::BeforePublish),
            ),
            SaveCommit::Unavailable { .. }
        ));
        let SaveLoad::Present(saved) = store.load() else {
            panic!("failed save replaced the checkpoint");
        };
        assert_eq!(saved.value, b"old");

        let uncertain_operation = OperationId::new();
        assert!(matches!(
            store.commit_with_fault(
                Some(first.revision),
                uncertain_operation,
                b"published",
                Some(CommitFault::AfterPublish),
            ),
            SaveCommit::Uncertain {
                operation_id,
                ..
            } if operation_id == uncertain_operation
        ));
        assert!(matches!(
            store.commit(Some(first.revision), OperationId::new(), b"new"),
            SaveCommit::Uncertain {
                operation_id,
                ..
            } if operation_id == uncertain_operation
        ));
        let SaveCommit::Committed(receipt) =
            store.commit(Some(first.revision), uncertain_operation, b"published")
        else {
            panic!("uncertain retry did not resolve exact publication");
        };
        assert_eq!(receipt.revision.value(), 2);
        assert!(store.uncertain.is_none());
        let SaveLoad::Present(saved) = store.load() else {
            panic!("resolved save did not reload");
        };
        assert_eq!(saved.value, b"published");
    }

    #[test]
    fn javascript_program_save_rejects_corruption_bounds_and_other_writers() {
        let (root, program, universe, slot) = fixture();
        let mut store =
            TransactionalSaveStore::open_test(root.directory(), program, universe, slot).unwrap();
        assert!(matches!(
            store.commit(None, OperationId::new(), &[0; MAX_PAYLOAD_BYTES + 1]),
            SaveCommit::Unavailable {
                kind: io::ErrorKind::InvalidInput,
                ..
            }
        ));
        assert!(matches!(
            TransactionalSaveStore::open_test(root.directory(), program, universe, SlotId::new()),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock
        ));

        private_storage::write_new_private(&store.directory, &store.save_name(), b"{broken")
            .unwrap();
        assert!(matches!(store.load(), SaveLoad::Corrupt(_)));
        assert!(matches!(
            store.commit(None, OperationId::new(), b"cannot overwrite"),
            SaveCommit::Corrupt(_)
        ));
        assert_eq!(
            &**private_storage::read_private(&store.directory, &store.save_name(), 16,).unwrap(),
            b"{broken"
        );
    }

    #[test]
    fn javascript_program_migration_backs_up_and_refuses_downward_publication() {
        let (root, program, universe, slot) = fixture();
        let mut store =
            TransactionalSaveStore::open_test(root.directory(), program, universe, slot).unwrap();
        let old_operation = OperationId::new();
        let SaveCommit::Committed(old) = store.commit_with_schema(
            None,
            old_operation,
            OLDEST_APPROVED_SAVE_SCHEMA,
            b"legacy",
            false,
            None,
        ) else {
            panic!("legacy fixture save was not committed");
        };
        let prior_bytes =
            private_storage::read_private(&store.directory, &store.save_name(), MAX_ENVELOPE_BYTES)
                .unwrap();
        assert!(matches!(
            store.commit(Some(old.revision), OperationId::new(), b"downward"),
            SaveCommit::Conflict { .. }
        ));

        let SaveCommit::Committed(migrated) = store.commit_migrating(
            Some(old.revision),
            OperationId::new(),
            CURRENT_SAVE_SCHEMA,
            b"migrated",
        ) else {
            panic!("explicit migration did not publish");
        };
        assert_eq!(migrated.revision.value(), 2);
        assert_eq!(
            private_storage::read_private(
                &store.directory,
                &store.migration_backup_name(old.revision.value()),
                MAX_ENVELOPE_BYTES,
            )
            .unwrap(),
            prior_bytes
        );
        let SaveLoad::Present(snapshot) = store.load() else {
            panic!("migrated save did not reload");
        };
        assert_eq!(snapshot.schema_version, CURRENT_SAVE_SCHEMA);
        assert_eq!(snapshot.value, b"migrated");
    }

    #[test]
    fn reviewed_save_host_reconciles_then_migrates_reopened_legacy_save() {
        let root = PrivateFixture::new();
        let program = ProgramId::from_host(DEVELOPER_PROGRAM_ID);
        let universe = UniverseId::from_host(DEVELOPER_UNIVERSE_ID);
        let slot = SlotId::from_host(DEVELOPER_SLOT_ID);
        {
            let mut store =
                TransactionalSaveStore::open_test(root.directory(), program, universe, slot)
                    .unwrap();
            assert!(matches!(
                store.commit_with_schema(
                    None,
                    OperationId::new(),
                    OLDEST_APPROVED_SAVE_SCHEMA,
                    b"legacy",
                    false,
                    None,
                ),
                SaveCommit::Committed(_)
            ));
        }
        let host = ReviewedDeveloperStorageHost::from_test(
            TransactionalSaveStore::open_test(root.directory(), program, universe, slot).unwrap(),
            ProgramLaunchMode::Resume,
        );
        assert_eq!(host.prepare(), Ok(()));
        assert!(matches!(
            host.commit(
                DEVELOPER_SLOT,
                ProgramStorageCommitRequest {
                    expected_revision: Some(1),
                    schema_version: CURRENT_SAVE_SCHEMA,
                    value: b"migrated".to_vec(),
                    operation_id: OperationId::new().0.hyphenated().to_string(),
                },
            ),
            ProgramStorageCommit::Committed { revision: 2 }
        ));
        let stored = host
            .store
            .lock()
            .unwrap()
            .as_ref()
            .expect("resume host retained its store")
            .load();
        assert!(matches!(
            stored,
            SaveLoad::Present(SaveSnapshot {
                revision: SaveRevision(2),
                schema_version: CURRENT_SAVE_SCHEMA,
                ..
            })
        ));
    }

    #[test]
    fn reviewed_save_host_distinguishes_new_resume_and_exclusive_writer() {
        let root = PrivateFixture::new();
        let new = ReviewedDeveloperStorageHost::from_test(
            TransactionalSaveStore::open_test(
                root.directory(),
                ProgramId::from_host(DEVELOPER_PROGRAM_ID),
                UniverseId::from_host(DEVELOPER_UNIVERSE_ID),
                SlotId::from_host(DEVELOPER_SLOT_ID),
            )
            .unwrap(),
            ProgramLaunchMode::New,
        );
        new.prepare().unwrap();
        assert_eq!(new.load(DEVELOPER_SLOT), ProgramStorageLoad::Missing);
        assert!(TransactionalSaveStore::open_test(
            root.directory(),
            ProgramId::from_host(DEVELOPER_PROGRAM_ID),
            UniverseId::from_host(DEVELOPER_UNIVERSE_ID),
            SlotId::from_host(DEVELOPER_SLOT_ID),
        )
        .is_err());
        let ProgramStorageCommit::Committed { revision } = new.commit(
            DEVELOPER_SLOT,
            ProgramStorageCommitRequest {
                expected_revision: None,
                schema_version: CURRENT_SAVE_SCHEMA,
                value: b"state".to_vec(),
                operation_id: Uuid::new_v4().hyphenated().to_string(),
            },
        ) else {
            panic!("initial reviewed save did not commit");
        };
        assert_eq!(revision, 1);
        drop(new);

        let duplicate = ReviewedDeveloperStorageHost::from_test(
            TransactionalSaveStore::open_test(
                root.directory(),
                ProgramId::from_host(DEVELOPER_PROGRAM_ID),
                UniverseId::from_host(DEVELOPER_UNIVERSE_ID),
                SlotId::from_host(DEVELOPER_SLOT_ID),
            )
            .unwrap(),
            ProgramLaunchMode::New,
        );
        assert_eq!(
            duplicate.prepare(),
            Err(ProgramStoragePreparation::ExistingSave)
        );
        drop(duplicate);
        let resumed = ReviewedDeveloperStorageHost::from_test(
            TransactionalSaveStore::open_test(
                root.directory(),
                ProgramId::from_host(DEVELOPER_PROGRAM_ID),
                UniverseId::from_host(DEVELOPER_UNIVERSE_ID),
                SlotId::from_host(DEVELOPER_SLOT_ID),
            )
            .unwrap(),
            ProgramLaunchMode::Resume,
        );
        resumed.prepare().unwrap();
        assert!(matches!(
            resumed.load(DEVELOPER_SLOT),
            ProgramStorageLoad::Present {
                schema_version: CURRENT_SAVE_SCHEMA,
                revision: 1,
                ..
            }
        ));
    }

    #[test]
    fn javascript_program_storage_reopen_retains_confirmed_save_for_explicit_resume() {
        let root = PrivateFixture::new();
        let identities = (
            ProgramId::from_host(DEVELOPER_PROGRAM_ID),
            UniverseId::from_host(DEVELOPER_UNIVERSE_ID),
            SlotId::from_host(DEVELOPER_SLOT_ID),
        );
        let initial = ReviewedDeveloperStorageHost::from_test(
            TransactionalSaveStore::open_test(
                root.directory(),
                identities.0,
                identities.1,
                identities.2,
            )
            .unwrap(),
            ProgramLaunchMode::New,
        );
        initial.prepare().unwrap();
        for (expected_revision, value) in [(None, b"confirmed-1"), (Some(1), b"confirmed-2")] {
            assert!(matches!(
                initial.commit(
                    DEVELOPER_SLOT,
                    ProgramStorageCommitRequest {
                        expected_revision,
                        schema_version: CURRENT_SAVE_SCHEMA,
                        value: value.to_vec(),
                        operation_id: OperationId::new().0.hyphenated().to_string(),
                    },
                ),
                ProgramStorageCommit::Committed { .. }
            ));
        }
        drop(initial);

        let fresh = ReviewedDeveloperStorageHost::from_test(
            TransactionalSaveStore::open_test(
                root.directory(),
                identities.0,
                identities.1,
                identities.2,
            )
            .expect("termination must make the universe writer available"),
            ProgramLaunchMode::Resume,
        );
        fresh.prepare().unwrap();
        assert_eq!(
            fresh.load(DEVELOPER_SLOT),
            ProgramStorageLoad::Present {
                schema_version: CURRENT_SAVE_SCHEMA,
                revision: 2,
                value: b"confirmed-2".to_vec(),
            },
            "fresh Resume sees only the last confirmed checkpoint, never prior in-memory state"
        );
    }

    #[test]
    fn reviewed_save_host_refuses_missing_resume() {
        let root = PrivateFixture::new();
        let host = ReviewedDeveloperStorageHost::from_test(
            TransactionalSaveStore::open_test(
                root.directory(),
                ProgramId::from_host(DEVELOPER_PROGRAM_ID),
                UniverseId::from_host(DEVELOPER_UNIVERSE_ID),
                SlotId::from_host(DEVELOPER_SLOT_ID),
            )
            .unwrap(),
            ProgramLaunchMode::Resume,
        );
        assert_eq!(host.prepare(), Err(ProgramStoragePreparation::MissingSave));
    }

    #[test]
    fn reviewed_save_host_refuses_future_schema_without_publication() {
        let root = PrivateFixture::new();
        let mut store = TransactionalSaveStore::open_test(
            root.directory(),
            ProgramId::from_host(DEVELOPER_PROGRAM_ID),
            UniverseId::from_host(DEVELOPER_UNIVERSE_ID),
            SlotId::from_host(DEVELOPER_SLOT_ID),
        )
        .unwrap();
        store.commit(None, OperationId::new(), b"current");
        let mut envelope: SaveEnvelope = serde_json::from_slice(
            &private_storage::read_private(
                &store.directory,
                &store.save_name(),
                MAX_ENVELOPE_BYTES,
            )
            .unwrap(),
        )
        .unwrap();
        envelope.schema_version = CURRENT_SAVE_SCHEMA + 1;
        private_storage::replace_private(
            &store.directory,
            &store.save_name(),
            &serde_json::to_vec(&envelope).unwrap(),
        )
        .unwrap();
        drop(store);

        let host = ReviewedDeveloperStorageHost::from_test(
            TransactionalSaveStore::open_test(
                root.directory(),
                ProgramId::from_host(DEVELOPER_PROGRAM_ID),
                UniverseId::from_host(DEVELOPER_UNIVERSE_ID),
                SlotId::from_host(DEVELOPER_SLOT_ID),
            )
            .unwrap(),
            ProgramLaunchMode::Resume,
        );
        assert_eq!(host.prepare(), Err(ProgramStoragePreparation::Future));
        assert!(matches!(
            host.load(DEVELOPER_SLOT),
            ProgramStorageLoad::Future {
                actual_schema_version
            } if actual_schema_version == CURRENT_SAVE_SCHEMA + 1
        ));
        assert!(matches!(
            host.commit(
                DEVELOPER_SLOT,
                ProgramStorageCommitRequest {
                    expected_revision: Some(1),
                    schema_version: CURRENT_SAVE_SCHEMA,
                    value: b"replacement".to_vec(),
                    operation_id: OperationId::new().0.hyphenated().to_string(),
                },
            ),
            ProgramStorageCommit::Future {
                actual_schema_version
            } if actual_schema_version == CURRENT_SAVE_SCHEMA + 1
        ));
        drop(host);
        let new = ReviewedDeveloperStorageHost::from_test(
            TransactionalSaveStore::open_test(
                root.directory(),
                ProgramId::from_host(DEVELOPER_PROGRAM_ID),
                UniverseId::from_host(DEVELOPER_UNIVERSE_ID),
                SlotId::from_host(DEVELOPER_SLOT_ID),
            )
            .unwrap(),
            ProgramLaunchMode::New,
        );
        assert_eq!(new.prepare(), Err(ProgramStoragePreparation::Future));
    }
}
