//! Bounded, host-owned transactional storage for JavaScript program saves.
//!
//! This core deliberately does not expose directory handles, paths, UI state,
//! or worker integration. A later program host must bind its opaque IDs to a
//! reviewed installed program and selected universe before constructing it.

use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

use festerm_sessiond::private_storage::{self, PreparedCheckpoint, UnlockOnDrop};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const STORAGE_VERSION: u8 = 1;
const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_ENVELOPE_BYTES: usize = 320 * 1024;
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
    pub(crate) value: Vec<u8>,
    operation_id: OperationId,
}

impl SaveSnapshot {
    pub(crate) fn operation_id(&self) -> OperationId {
        self.operation_id
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

    pub(crate) fn commit(
        &mut self,
        expected_revision: Option<SaveRevision>,
        operation_id: OperationId,
        payload: &[u8],
    ) -> SaveCommit {
        self.commit_with_fault(expected_revision, operation_id, payload, None)
    }

    fn commit_with_fault(
        &mut self,
        expected_revision: Option<SaveRevision>,
        operation_id: OperationId,
        payload: &[u8],
        fault: Option<CommitFault>,
    ) -> SaveCommit {
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
            let parent = File::open(path.parent().unwrap()).unwrap();
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
}
