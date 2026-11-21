use serde::{Deserialize, Serialize};

/// A value comfortably below the 65,536-byte IPC framing ceiling even after
/// JSON serialization.
pub const MAX_STORAGE_VALUE_BYTES: usize = 12 * 1024;
pub const MAX_STORAGE_SLOT_BYTES: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProgramStorageLoad {
    Missing,
    Present {
        schema_version: u32,
        revision: u64,
        value: Vec<u8>,
    },
    Corrupt,
    Future {
        actual_schema_version: u32,
    },
    Unavailable,
    BusyWriter,
    ResourceLimit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProgramStorageSnapshot {
    pub schema_version: u32,
    pub revision: u64,
    pub value: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProgramStorageCommit {
    Committed { revision: u64 },
    Conflict { actual_revision: Option<u64> },
    Corrupt,
    Future { actual_schema_version: u32 },
    Unavailable,
    BusyWriter,
    Uncertain,
    ResourceLimit,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProgramStorageCommitRequest {
    pub expected_revision: Option<u64>,
    pub schema_version: u32,
    pub value: Vec<u8>,
    pub operation_id: String,
}

/// Trusted, application-injected save boundary. Opaque slots are interpreted
/// only by the host; engine and worker code never receive paths or handles.
pub trait ProgramStorageHost: Send + Sync {
    fn prepare(&self) -> Result<(), ProgramStoragePreparation> {
        Ok(())
    }

    fn load(&self, slot: &str) -> ProgramStorageLoad;
    fn commit(&self, slot: &str, request: ProgramStorageCommitRequest) -> ProgramStorageCommit;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgramStoragePreparation {
    ExistingSave,
    MissingSave,
    BusyWriter,
    Corrupt,
    Future,
    UnapprovedSchema,
    Unavailable,
}

impl ProgramStoragePreparation {
    pub const fn message(self) -> &'static str {
        match self {
            Self::ExistingSave => "A save already exists; choose Resume instead of Start.",
            Self::MissingSave => "No saved state exists for this universe.",
            Self::BusyWriter => "This save is already open in another writer.",
            Self::Corrupt => "The save is corrupt and was not changed.",
            Self::Future => "The save belongs to a newer, unsupported schema.",
            Self::UnapprovedSchema => "The save schema is not approved for this program.",
            Self::Unavailable => "Program save storage is unavailable.",
        }
    }
}

#[derive(Default)]
pub struct UnavailableProgramStorage;

impl ProgramStorageHost for UnavailableProgramStorage {
    fn load(&self, _slot: &str) -> ProgramStorageLoad {
        ProgramStorageLoad::Unavailable
    }

    fn commit(&self, _slot: &str, _request: ProgramStorageCommitRequest) -> ProgramStorageCommit {
        ProgramStorageCommit::Unavailable
    }
}
