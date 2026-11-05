//! Portable host/client contract for rooted filesystem streams (ADR 0067).
use crate::protocol::Target;
use serde::{Deserialize, Serialize};

pub const VERSION: u16 = 1;
pub const DISCOVERY_VERSION: u16 = 2;
pub const OPEN: u8 = 30;
pub const REQUEST: u8 = 31;
pub const RESPONSE: u8 = 32;
pub const DATA: u8 = 33;
pub const CHUNK_LIMIT: usize = 64 * 1024;
pub const JSON_LIMIT: usize = 64 * 1024;
pub const PAGE_LIMIT: u16 = 64;
pub const HANDLE_LIMIT: usize = 8;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub root: std::path::PathBuf,
    pub write: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub version: u16,
    pub write: bool,
    pub chunk_limit: usize,
    pub page_limit: u16,
    pub handle_limit: usize,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Open {
    pub version: u16,
    pub target: Target,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Discovery {
    pub version: u16,
    pub target: Target,
    pub filesystem: Option<Capabilities>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: u32,
    pub operation: Operation,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Stat {
        path: String,
    },
    OpenDirectory {
        path: String,
    },
    ReadDirectory {
        handle: u32,
        limit: u16,
    },
    OpenRead {
        path: String,
    },
    Read {
        handle: u32,
        offset: String,
        length: u32,
    },
    BeginUpload {
        path: String,
        size: String,
        expected: Option<u32>,
    },
    Write {
        handle: u32,
        offset: String,
        length: u32,
    },
    FinishUpload {
        handle: u32,
    },
    Close {
        handle: u32,
    },
    Mkdir {
        path: String,
    },
    Ping {},
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    File,
    Directory,
    Unsupported,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub kind: Kind,
    pub size: String,
    pub modified_ms: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Result {
    Ready {
        capabilities: Capabilities,
    },
    Stat {
        metadata: Metadata,
    },
    Directory {
        handle: u32,
    },
    Entries {
        entries: Vec<Entry>,
        done: bool,
        skipped: u16,
    },
    File {
        handle: u32,
        metadata: Metadata,
    },
    Read {
        offset: String,
        length: u32,
        eof: bool,
    },
    Upload {
        handle: u32,
    },
    Written {
        offset: String,
    },
    Finished {
        size: String,
    },
    Closed,
    Created,
    Pong,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Invalid,
    Denied,
    NotFound,
    Exists,
    Conflict,
    Busy,
    Io,
    RecoveryRequired,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Result>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_path: Option<String>,
}

pub fn decimal(value: &str) -> Option<u64> {
    if value.is_empty()
        || value.len() > 20
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    value.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_requests_refuse_ambiguous_sizes_unknown_fields_and_oversized_frames() {
        for value in ["", "-1", "+1", "01", " 1", "1.0", "18446744073709551616"] {
            assert!(decimal(value).is_none());
        }
        assert_eq!(decimal("18446744073709551615"), Some(u64::MAX));
        assert!(serde_json::from_str::<Request>(
            r#"{"id":1,"operation":{"op":"delete","path":"/a"}}"#
        )
        .is_err());
        assert!(serde_json::from_str::<Request>(
            r#"{"id":1,"operation":{"op":"ping","shell":"bad"}}"#
        )
        .is_err());
        assert!(crate::protocol::validate_length(DATA, CHUNK_LIMIT + 5).is_err());
        assert!(crate::protocol::validate_length(REQUEST, JSON_LIMIT + 1).is_err());
    }
}
