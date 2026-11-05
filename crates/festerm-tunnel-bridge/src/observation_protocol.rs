//! Portable, independently consented read-only observation protocol (ADR 0070).
use crate::{file_protocol, protocol::Target};
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observation_strict_schemas_lengths_and_canonical_offsets_preserve_old_discovery() {
        for value in ["", "01", "-1", "+1", "1.0", "18446744073709551616"] {
            assert!(decimal(value).is_err());
        }
        assert_eq!(decimal("18446744073709551615").unwrap(), u64::MAX);
        for (kind, maximum) in [
            (OPEN, 4096),
            (REQUEST, JSON_LIMIT),
            (RESPONSE, JSON_LIMIT),
            (DATA, CHUNK_LIMIT + 4),
            (NOTICE, JSON_LIMIT),
        ] {
            assert!(crate::protocol::validate_length(kind, maximum).is_ok());
            assert!(crate::protocol::validate_length(kind, maximum + 1).is_err());
        }
        assert!(serde_json::from_str::<Request>(
            r#"{"id":1,"root":"r","operation":{"op":"write","path":"/x"}}"#
        )
        .is_err());
        assert!(serde_json::from_str::<Request>(
            r#"{"id":1,"root":"r","operation":{"op":"stat","path":"/x","shell":true}}"#
        )
        .is_err());
        assert!(serde_json::from_str::<file_protocol::Discovery>(r#"{"version":2,"target":{"name":"x","pid":1,"generation":"2"},"filesystem":null,"observation":null}"#).is_err());
        assert!(serde_json::from_str::<crate::protocol::Description>(r#"{"version":1,"target":{"name":"x","pid":1,"generation":"2"},"files":{"read":false,"windows_paths":false},"observation":null}"#).is_err());
    }
}

pub const VERSION: u16 = 1;
pub const DISCOVERY_VERSION: u16 = 3;
pub const OPEN: u8 = 40;
pub const REQUEST: u8 = 41;
pub const RESPONSE: u8 = 42;
pub const DATA: u8 = 43;
pub const NOTICE: u8 = 44;
pub const ROOT_LIMIT: usize = 4;
pub const CHUNK_LIMIT: usize = 65536;
pub const JSON_LIMIT: usize = 65536;
pub const PAGE_LIMIT: u16 = 64;
pub const FILE_LIMIT: usize = 64;
pub const CURSOR_LIMIT: usize = 8;
pub const WATCH_DIRECTORY_LIMIT: usize = 32;
pub const NOTICE_LIMIT: usize = 64;
pub const STREAM_LIMIT: usize = 4;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub roots: Vec<std::path::PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Root {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub version: u16,
    pub roots: Vec<Root>,
    pub chunk_limit: usize,
    pub page_limit: u16,
    pub watch: bool,
    pub correlation: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Discovery {
    pub version: u16,
    pub target: Target,
    pub filesystem: Option<file_protocol::Capabilities>,
    pub observation: Option<Capabilities>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Open {
    pub version: u16,
    pub target: Target,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    File,
    Directory,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub kind: Kind,
    pub identity: String,
    pub size: String,
    pub modified_ms: Option<String>,
    pub revision: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub name: String,
    pub metadata: Metadata,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub entries: Vec<Entry>,
    pub done: bool,
    /// Unreadable, unsupported, vanished or unrepresentable native entries.
    pub skipped: u16,
}

pub type DirectoryCursor = String;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Correlation {
    pub value: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: u32,
    pub root: String,
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
        cursor: DirectoryCursor,
        limit: u16,
    },
    CloseDirectory {
        cursor: DirectoryCursor,
    },
    Read {
        path: String,
        offset: String,
        length: u32,
        expected: Metadata,
    },
    Watch {},
    Correlation {},
    Ping {},
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Result {
    Ready { capabilities: Capabilities },
    Stat { metadata: Metadata },
    Directory { cursor: DirectoryCursor },
    Page { page: Page },
    Closed,
    Data { metadata: Metadata, length: u32 },
    Changed { metadata: Metadata },
    Watching,
    Correlation { correlation: Option<Correlation> },
    Pong,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub id: u32,
    pub result: Option<Result>,
    pub error: Option<file_protocol::ErrorCode>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum WatchEvent {
    Created { path: String },
    Modified { path: String },
    Removed { path: String },
    Renamed { from: String, to: String },
    RescanRequired,
    Ended,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WatchNotice {
    pub sequence: String,
    pub event: WatchEvent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadOutcome {
    Data { metadata: Metadata, bytes: Vec<u8> },
    Changed { metadata: Metadata },
}

pub fn decimal(value: &str) -> std::io::Result<u64> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| crate::protocol::invalid())?;
    if parsed.to_string() != value {
        return Err(crate::protocol::invalid());
    }
    Ok(parsed)
}

pub fn valid_path(path: &str) -> bool {
    if path == "/" {
        return true;
    }
    path.len() <= 4096
        && path.starts_with('/')
        && path[1..].split('/').all(|name| {
            !name.is_empty()
                && name.len() <= 255
                && !matches!(name, "." | "..")
                && !name.chars().any(|c| c.is_control() || c == '\\')
        })
}

impl Capabilities {
    pub(crate) fn validate(&self) -> std::io::Result<()> {
        if self.version != VERSION
            || self.roots.is_empty()
            || self.roots.len() > ROOT_LIMIT
            || self.chunk_limit == 0
            || self.chunk_limit > CHUNK_LIMIT
            || self.page_limit == 0
            || self.page_limit > PAGE_LIMIT
        {
            return Err(crate::protocol::invalid());
        }
        let mut ids = std::collections::HashSet::new();
        for root in &self.roots {
            if root.id.len() != 32
                || !root.id.bytes().all(|c| c.is_ascii_hexdigit())
                || root.label.len() > 255
                || root.label.chars().any(char::is_control)
                || !ids.insert(&root.id)
            {
                return Err(crate::protocol::invalid());
            }
        }
        Ok(())
    }
}

impl Metadata {
    pub(crate) fn validate(&self) -> std::io::Result<()> {
        decimal(&self.size)?;
        if let Some(time) = &self.modified_ms {
            decimal(time)?;
        }

        if self.identity.is_empty()
            || self.identity.len() > 128
            || self.revision.len() != 32
            || !self.revision.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err(crate::protocol::invalid());
        }
        Ok(())
    }
}
