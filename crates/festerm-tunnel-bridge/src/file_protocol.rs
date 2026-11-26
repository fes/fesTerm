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

pub const LOCATION_LIMIT: usize = 8;
pub const POLICY_BYTES_LIMIT: usize = 8 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub struct Policy {
    /// Historical singleton initial directory. Never mixed with locations.
    pub start_in: Option<String>,
    /// Historical root, or a derived mirror of the first allowlist entry.
    /// The mirror is never serialized in the new shape and adds no grant.
    pub root: std::path::PathBuf,
    pub locations: Option<AllowedLocations>,
    pub write: bool,
    pub delete: bool,
}
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AllowedLocations {
    #[serde(deserialize_with = "bounded_location_roots")]
    pub roots: Vec<std::path::PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<LocationStart>,
}
impl std::fmt::Debug for AllowedLocations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AllowedLocations")
            .field("roots", &"[private folders]")
            .field("start", &"[private folder]")
            .finish()
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LocationStart {
    /// Protected list ordinal, not an ephemeral wire selector.
    pub root: usize,
    pub path: StartDirectory,
}
fn bounded_location_roots<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> std::result::Result<Vec<std::path::PathBuf>, D::Error> {
    struct Roots;
    impl<'de> serde::de::Visitor<'de> for Roots {
        type Value = Vec<std::path::PathBuf>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("one to eight allowed locations")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut roots = Vec::new();
            while let Some(root) = seq.next_element()? {
                if roots.len() >= LOCATION_LIMIT {
                    return Err(serde::de::Error::custom("too many allowed locations"));
                }
                roots.push(root);
            }
            if roots.is_empty() {
                return Err(serde::de::Error::custom("no allowed locations"));
            }
            Ok(roots)
        }
    }
    decoder.deserialize_seq(Roots)
}
impl Serialize for Policy {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct(
            "Policy",
            if self.locations.is_some() {
                2 + usize::from(self.delete)
            } else {
                2 + usize::from(self.start_in.is_some()) + usize::from(self.delete)
            },
        )?;
        if let Some(locations) = &self.locations {
            state.serialize_field("locations", locations)?;
        } else {
            // Preserve the historical bytes and field order exactly.
            if let Some(start) = &self.start_in {
                state.serialize_field("start_in", start)?;
            }
            state.serialize_field("root", &self.root)?;
        }
        state.serialize_field("write", &self.write)?;
        if !is_false(&self.delete) {
            state.serialize_field("delete", &self.delete)?;
        }
        state.end()
    }
}
impl<'de> Deserialize<'de> for Policy {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> std::result::Result<Self, D::Error> {
        fn present<'input, T: Deserialize<'input>, D: serde::Deserializer<'input>>(
            decoder: D,
        ) -> std::result::Result<Option<T>, D::Error> {
            T::deserialize(decoder).map(Some)
        }

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Shape {
            // Historical start_in accepts null, but its presence excludes locations.
            #[serde(default, deserialize_with = "present")]
            start_in: Option<Option<String>>,
            #[serde(default, deserialize_with = "present")]
            root: Option<std::path::PathBuf>,
            #[serde(default, deserialize_with = "present")]
            locations: Option<AllowedLocations>,
            write: bool,
            #[serde(default)]
            delete: bool,
        }
        let shape = Shape::deserialize(decoder)?;
        let root = match (shape.root, &shape.locations) {
            (Some(root), None) => root,
            (None, Some(locations)) if shape.start_in.is_none() => locations
                .roots
                .first()
                .cloned()
                .ok_or_else(|| serde::de::Error::custom("no allowed locations"))?,
            _ => return Err(serde::de::Error::custom("mixed or missing file scope")),
        };
        let policy = Self {
            root,
            start_in: shape.start_in.flatten(),
            locations: shape.locations,
            write: shape.write,
            delete: shape.delete,
        };
        if policy.locations.is_some() {
            policy
                .validate()
                .map_err(|_| serde::de::Error::custom("invalid allowed locations"))?;
        }
        Ok(policy)
    }
}
fn valid_location_root(root: &std::path::Path) -> bool {
    let Some(text) = root.to_str() else {
        return false;
    };
    root.is_absolute()
        && text.len() <= 4096
        && !text.contains("//")
        && !text.contains("\\\\")
        && !text
            .split(['/', '\\'])
            .any(|part| matches!(part, "." | ".."))
        && root.components().all(|part| match part {
            std::path::Component::Normal(name) => name.to_str().is_some_and(valid_name),
            std::path::Component::RootDir => true,
            std::path::Component::Prefix(prefix) => {
                matches!(prefix.kind(), std::path::Prefix::Disk(_))
            }
            _ => false,
        })
}
fn is_false(value: &bool) -> bool {
    !value
}

impl std::fmt::Debug for Policy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Policy")
            .field("root", &"[private folder]")
            .field("start_in", &"[private folder]")
            .field("locations", &"[private folders]")
            .field("write", &self.write)
            .field("delete", &self.delete)
            .finish()
    }
}
impl Policy {
    pub fn initial_directory(&self) -> &str {
        self.start_in.as_deref().unwrap_or("/")
    }

    /// Portable shape only. Native admission separately opens and verifies the
    /// root and ancestors without following links or repairing permissions.
    pub fn validate(&self) -> std::io::Result<()> {
        if let Some(locations) = &self.locations {
            if self.start_in.is_some()
                || locations.roots.is_empty()
                || locations.roots.len() > LOCATION_LIMIT
                || locations.roots.first() != Some(&self.root)
                || locations
                    .roots
                    .iter()
                    .any(|root| !valid_location_root(root))
                || locations
                    .roots
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != locations.roots.len()
                || locations.start.as_ref().is_some_and(|start| {
                    start.root >= locations.roots.len() || start.path.as_str().len() > 4096 - 33
                })
                || serde_json::to_vec(self)
                    .map_err(|_| crate::protocol::invalid())?
                    .len()
                    > POLICY_BYTES_LIMIT
            {
                return Err(crate::protocol::invalid());
            }
        }
        if self.start_in.as_ref().is_some_and(|path| !valid_path(path))
            || self.delete && !self.write
            || !self.root.is_absolute()
            || self.root.to_str().is_none_or(|p| p.len() > 4096)
            || self
                .root
                .components()
                .any(|p| matches!(p, std::path::Component::ParentDir))
        {
            return Err(crate::protocol::invalid());
        }
        Ok(())
    }
}

pub const HOST_VERSION: u16 = 2;
pub const HOST_DESCRIPTION: u8 = 34;
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostScope {
    pub host: festerm_host_protocol::HostId,
    pub installation: festerm_host_protocol::HostId,
    pub epoch: String,
}
impl HostScope {
    pub fn validate(&self) -> std::io::Result<()> {
        if decimal(&self.epoch).is_none_or(|epoch| epoch == 0) {
            return Err(crate::protocol::invalid());
        }
        Ok(())
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostDescription {
    pub version: u16,
    pub scope: HostScope,
    pub filesystem: Option<Capabilities>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostOpen {
    pub version: u16,
    pub scope: HostScope,
}
/// Current protected facts, never a serialized client credential or session grant.
#[derive(Clone, Eq, PartialEq)]
pub struct HostPolicy {
    pub scope: HostScope,
    pub filesystem: Option<Policy>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub version: u16,
    pub write: bool,
    pub chunk_limit: usize,
    pub page_limit: u16,
    pub handle_limit: usize,
}

/// Explicit HOST opening mode. Legacy/Open2 greetings and replies stay v1.
pub const HOST_REVIEWED_VERSION: u16 = 3;
/// Explicit new namespace; historical clients must never reinterpret it.
pub const HOST_LOCATIONS_VERSION: u16 = 4;
pub const OPERATION_LIMIT: usize = 16;
pub const OPERATION_NAME_LIMIT: usize = 32;
pub const READ_LINKS_OPERATION: &str = "read_links";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperationCapabilities {
    pub version: u16,
    pub write: bool,
    #[serde(deserialize_with = "bounded_operations")]
    pub operations: Vec<String>,
    pub chunk_limit: usize,
    pub page_limit: u16,
    pub handle_limit: usize,
}
fn bounded_operations<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> std::result::Result<Vec<String>, D::Error> {
    struct Names;
    impl<'de> serde::de::Visitor<'de> for Names {
        type Value = Vec<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a bounded list of optional operation names")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut names = Vec::new();
            while let Some(name) = seq.next_element::<String>()? {
                if names.len() == OPERATION_LIMIT
                    || name.is_empty()
                    || name.len() > OPERATION_NAME_LIMIT
                    || name.chars().any(char::is_control)
                {
                    return Err(serde::de::Error::custom(
                        "operation names exceed their bound",
                    ));
                }
                names.push(name);
            }
            Ok(names)
        }
    }
    decoder.deserialize_seq(Names)
}
impl OperationCapabilities {
    /// Unknown optional names are inert; protected policy remains strictly typed.
    pub fn allows_locations(&self) -> bool {
        self.operations.iter().any(|name| name == "locations")
    }
    pub fn allows_start_directory(&self) -> bool {
        self.operations.iter().any(|name| name == "start_directory")
    }
    pub fn allows_display_root(&self) -> bool {
        self.operations.iter().any(|name| name == "display_root")
    }
    pub fn allows_delete(&self) -> bool {
        self.operations.iter().any(|name| name == "delete")
    }
    pub fn allows_read_links(&self) -> bool {
        self.operations
            .iter()
            .any(|name| name == READ_LINKS_OPERATION)
    }
    pub fn validate(&self, base: &Capabilities) -> bool {
        self.version == HOST_REVIEWED_VERSION
            && self.write == base.write
            && self.chunk_limit == base.chunk_limit
            && self.page_limit == base.page_limit
            && self.handle_limit == base.handle_limit
            && self.operations.len() <= OPERATION_LIMIT
            && self.operations.iter().all(|name| {
                !name.is_empty()
                    && name.len() <= OPERATION_NAME_LIMIT
                    && !name.chars().any(char::is_control)
            })
            && self
                .operations
                .iter()
                .filter(|name| name.as_str() == "delete")
                .count()
                <= 1
            && self
                .operations
                .iter()
                .filter(|name| name.as_str() == "display_root")
                .count()
                <= 1
            && self
                .operations
                .iter()
                .filter(|name| name.as_str() == "start_directory")
                .count()
                <= 1
            && self
                .operations
                .iter()
                .filter(|name| name.as_str() == READ_LINKS_OPERATION)
                .count()
                <= 1
            && (!self.allows_read_links() || self.allows_locations())
            && (!self.allows_delete() || self.write)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewPurpose {
    Delete,
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

/// Bounded virtual initial location; Debug never discloses the private path.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct StartDirectory(String);
impl StartDirectory {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for StartDirectory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StartDirectory([redacted])")
    }
}
impl TryFrom<String> for StartDirectory {
    type Error = &'static str;
    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        if !valid_path(&value) {
            return Err("invalid initial directory");
        }
        Ok(Self(value))
    }
}
impl From<StartDirectory> for String {
    fn from(value: StartDirectory) -> Self {
        value.0
    }
}

/// Authorized presentation metadata only; never a filesystem request path.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DisplayRoot(String);
impl DisplayRoot {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for DisplayRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DisplayRoot([redacted])")
    }
}
impl TryFrom<String> for DisplayRoot {
    type Error = &'static str;
    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        let bytes = value.as_bytes();
        let absolute = value.starts_with('/')
            || (bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'/' | b'\\'))
            || value.strip_prefix("\\\\").is_some_and(|unc| {
                let mut parts = unc.split('\\');
                parts.next().is_some_and(|part| !part.is_empty())
                    && parts.next().is_some_and(|part| !part.is_empty())
            });
        if !absolute || value.len() > 4096 || value.chars().any(char::is_control) {
            return Err("invalid display root");
        }
        Ok(Self(value))
    }
}
impl From<DisplayRoot> for String {
    fn from(value: DisplayRoot) -> Self {
        value.0
    }
}

/// Public view metadata only; selectors are connection/grant-scoped virtual
/// path components. They are never native paths or persistent authority.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileLocation {
    pub selector: String,
    pub label: DisplayRoot,
    pub available: bool,
    /// Existing strict absolute-root chain qualifies. This is availability,
    /// not per-root consent or a guarantee of target OS write permission.
    pub mutations_available: bool,
}
impl FileLocation {
    pub fn validate(&self) -> bool {
        self.selector.len() == 32
            && self
                .selector
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
    pub fn virtual_root(&self) -> String {
        format!("/{}", self.selector)
    }
    pub fn contains(&self, path: &str) -> bool {
        path.strip_prefix(&self.virtual_root())
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }
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
    Locations {},
    StartDirectory {},
    DisplayRoot {},
    ReviewEntry {
        path: String,
        purpose: ReviewPurpose,
    },
    Delete {
        review: u32,
    },
    Stat {
        path: String,
    },
    StatReadable {
        path: String,
    },
    OpenDirectory {
        path: String,
    },
    OpenReadableDirectory {
        path: String,
    },
    ReadDirectory {
        handle: u32,
        limit: u16,
    },
    OpenRead {
        path: String,
    },
    OpenReadableFile {
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
#[serde(deny_unknown_fields)]
pub struct ReadableEntry {
    pub name: String,
    pub kind: Kind,
    pub read_only: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Result {
    Locations {
        locations: Vec<FileLocation>,
    },
    StartDirectory {
        path: StartDirectory,
    },
    DisplayRoot {
        root: DisplayRoot,
    },
    ReadyV3 {
        capabilities: OperationCapabilities,
    },
    EntryReview {
        handle: u32,
        metadata: Metadata,
    },
    Deleted,
    Ready {
        capabilities: Capabilities,
    },
    Stat {
        metadata: Metadata,
    },
    ReadableStat {
        metadata: Metadata,
        read_only: bool,
    },
    Directory {
        handle: u32,
    },
    ReadableDirectory {
        handle: u32,
        metadata: Metadata,
        read_only: bool,
    },
    Entries {
        entries: Vec<Entry>,
        done: bool,
        skipped: u16,
    },
    ReadableEntries {
        entries: Vec<ReadableEntry>,
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
    NotEmpty,
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

/// Validates the existing virtual rooted namespace on either host or client.
pub(crate) fn valid_path(path: &str) -> bool {
    path == "/"
        || path.len() <= 4096 && path.starts_with('/') && path[1..].split('/').all(valid_name)
}

pub(crate) fn valid_name(name: &str) -> bool {
    if name.is_empty()
        || name.len() > 255
        || matches!(name, "." | "..")
        || name.to_ascii_lowercase().starts_with(".festerm-transfer-")
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        || name.ends_with(['.', ' '])
    {
        return false;
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) && !(stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.as_bytes()[3].is_ascii_digit())
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
    fn allowed_locations_shape_is_explicit_bounded_and_preserves_legacy_bytes() {
        let roots = vec![
            std::path::PathBuf::from(if cfg!(windows) { "C:\\Users" } else { "/home" }),
            std::path::PathBuf::from(if cfg!(windows) { "D:\\Data" } else { "/data" }),
        ];
        let mut policy = Policy {
            root: roots[0].clone(),
            start_in: None,
            locations: Some(AllowedLocations {
                roots: roots.clone(),
                start: Some(LocationStart {
                    root: 1,
                    path: "/عربي".to_owned().try_into().unwrap(),
                }),
            }),
            write: true,
            delete: true,
        };
        policy.validate().unwrap();
        let bytes = serde_json::to_vec(&policy).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(value.get("root").is_none());
        assert!(value.get("start_in").is_none());
        assert_eq!(serde_json::from_slice::<Policy>(&bytes).unwrap(), policy);
        let mut mixed = value.clone();
        mixed["root"] = serde_json::to_value(&roots[0]).unwrap();
        assert!(serde_json::from_value::<Policy>(mixed).is_err());
        for field in ["root", "start_in"] {
            let mut mixed = value.clone();
            mixed[field] = serde_json::Value::Null;
            assert!(serde_json::from_value::<Policy>(mixed).is_err(), "{field}");
        }
        let mut changed = policy.clone();
        changed
            .locations
            .as_mut()
            .unwrap()
            .start
            .as_mut()
            .unwrap()
            .root = 0;
        assert_ne!(policy, changed);
        policy.locations.as_mut().unwrap().roots = vec![roots[0].clone(); 2];
        assert!(policy.validate().is_err());
        let mut too_many = value;
        too_many["locations"]["roots"] =
            serde_json::to_value(vec![&roots[0]; LOCATION_LIMIT + 1]).unwrap();
        assert!(serde_json::from_value::<Policy>(too_many).is_err());
        assert!(!format!("{policy:?}").contains("home"));
        assert!(!format!("{:?}", policy.locations).contains("Users"));
    }

    #[test]
    fn start_directory_policy_defaults_preserve_legacy_and_validate_virtual_paths() {
        let old = serde_json::json!({"root": if cfg!(windows) {"C:\\Users"} else {"/home"}, "write": false});
        let mut policy: Policy = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(policy.initial_directory(), "/");
        assert_eq!(serde_json::to_value(&policy).unwrap(), old);
        let mut explicit_null = old.clone();
        explicit_null["start_in"] = serde_json::Value::Null;
        assert_eq!(
            serde_json::from_value::<Policy>(explicit_null).unwrap(),
            policy
        );
        let mut mixed = old.clone();
        mixed["locations"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<Policy>(mixed).is_err());
        assert_eq!(
            serde_json::to_string(&policy).unwrap(),
            format!(
                r#"{{"root":{},"write":false}}"#,
                serde_json::to_string(&policy.root).unwrap()
            ),
            "omitted field keeps historical bytes and order"
        );
        for bad in [
            "relative",
            "/../escape",
            "/a/./b",
            "/a//b",
            "/a/",
            "/.festerm-transfer-x",
            "/CON",
            "/a\\b",
            "/~",
            "/a:",
        ] {
            policy.start_in = Some(bad.into());
            // ~ is a legal literal directory name on the wire, not owner shorthand.
            assert_eq!(policy.validate().is_ok(), bad == "/~", "{bad}");
        }
        policy.start_in = Some(format!("/{}", "x".repeat(256)));
        assert!(policy.validate().is_err());
        let reply = Result::StartDirectory {
            path: "/Documents/عربي".to_owned().try_into().unwrap(),
        };
        assert!(!format!("{reply:?}").contains("Documents"));
        policy.start_in = Some("/Documents/عربي".into());
        policy.validate().unwrap();
        assert!(!format!("{policy:?}").contains("Documents"));
        let changed = policy.clone();
        policy.start_in = None;
        assert_ne!(
            policy, changed,
            "complete Policy equality includes initial directory"
        );
    }

    #[test]
    fn display_root_is_portable_bounded_and_redacted() {
        for value in [
            "/Users/example",
            "C:\\Users\\example",
            "\\\\server\\share",
            "/",
        ] {
            let root = DisplayRoot::try_from(value.to_owned()).unwrap();
            assert_eq!(root.as_str(), value);
            let reply = Result::DisplayRoot { root };
            let encoded = serde_json::to_vec(&reply).unwrap();
            assert!(serde_json::from_slice::<Result>(&encoded).is_ok());
            assert!(!format!("{reply:?}").contains(value));
        }
        for value in [
            "relative".to_owned(),
            "C:relative".into(),
            "\\\\".into(),
            "\\\\server".into(),
            "/bad\npath".into(),
            format!("/{}", "x".repeat(4096)),
        ] {
            assert!(DisplayRoot::try_from(value.clone()).is_err());
            let encoded = serde_json::json!({"type":"display_root", "root":value});
            assert!(serde_json::from_value::<Result>(encoded).is_err());
        }
        assert!(DisplayRoot::try_from(format!("/{}", "x".repeat(4095))).is_ok());
    }

    #[test]
    fn display_root_keeps_historical_greetings_strict_and_optional_names_inert() {
        // Frozen ReadyV3/capability definitions from 2815f9fc: the historical
        // decoder knows no display-root field or operation semantics.
        #[derive(Deserialize, Serialize)]
        #[serde(deny_unknown_fields)]
        struct OldCapabilities {
            version: u16,
            write: bool,
            #[serde(deserialize_with = "bounded_operations")]
            operations: Vec<String>,
            chunk_limit: usize,
            page_limit: u16,
            handle_limit: usize,
        }
        #[derive(Deserialize, Serialize)]
        #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
        enum OldResult {
            ReadyV3 { capabilities: OldCapabilities },
        }
        let old = br#"{"type":"ready_v3","capabilities":{"version":3,"write":false,"operations":[],"chunk_limit":65536,"page_limit":64,"handle_limit":8}}"#;
        let old_value: serde_json::Value = serde_json::from_slice(old).unwrap();
        let current_reads_old: Result = serde_json::from_slice(old).unwrap();
        assert_eq!(serde_json::to_vec(&current_reads_old).unwrap(), old);
        // Use actual constants so this fixture guards the shape, not an
        // invented capability negotiation.
        let capabilities = OperationCapabilities {
            version: HOST_REVIEWED_VERSION,
            write: false,
            operations: vec![],
            chunk_limit: CHUNK_LIMIT,
            page_limit: PAGE_LIMIT,
            handle_limit: HANDLE_LIMIT,
        };
        let greeting = Result::ReadyV3 {
            capabilities: capabilities.clone(),
        };
        let bytes = serde_json::to_vec(&greeting).unwrap();
        let historical: OldResult = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(serde_json::to_vec(&historical).unwrap(), bytes);
        assert!(serde_json::from_value::<OldResult>(old_value).is_ok());
        let mut advertised = serde_json::to_value(&greeting).unwrap();
        advertised["capabilities"]["operations"] = serde_json::json!([
            "display_root",
            "locations",
            "start_directory",
            READ_LINKS_OPERATION
        ]);
        let OldResult::ReadyV3 {
            capabilities: old_caps,
        } = serde_json::from_value::<OldResult>(advertised.clone()).unwrap();
        assert_eq!(
            old_caps.operations,
            [
                "display_root",
                "locations",
                "start_directory",
                READ_LINKS_OPERATION
            ]
        );
        assert!(!old_caps.operations.iter().any(|name| name == "delete"));
        assert!(serde_json::from_value::<Result>(advertised).is_ok());
        let mut unsolicited = serde_json::to_value(greeting).unwrap();
        unsolicited["display_root"] = "/Users/example".into();
        assert!(serde_json::from_value::<OldResult>(unsolicited).is_err());
    }

    #[test]
    fn readable_link_wire_variants_are_additive_and_strict() {
        for request in [
            r#"{"op":"stat","path":"/owned"}"#,
            r#"{"op":"open_directory","path":"/owned"}"#,
            r#"{"op":"open_read","path":"/owned"}"#,
            r#"{"op":"read_directory","handle":1,"limit":1}"#,
            r#"{"op":"stat_readable","path":"/owned"}"#,
            r#"{"op":"open_readable_directory","path":"/owned"}"#,
            r#"{"op":"open_readable_file","path":"/owned"}"#,
        ] {
            let operation: Operation = serde_json::from_str(request).unwrap();
            assert_eq!(serde_json::to_string(&operation).unwrap(), request);
        }
        for reply in [
            r#"{"type":"stat","metadata":{"kind":"file","size":"1","modified_ms":null}}"#,
            r#"{"type":"directory","handle":1}"#,
            r#"{"type":"entries","entries":[{"name":"owned","kind":"file"}],"done":true,"skipped":0}"#,
            r#"{"type":"file","handle":1,"metadata":{"kind":"file","size":"1","modified_ms":null}}"#,
            r#"{"type":"readable_stat","metadata":{"kind":"file","size":"1","modified_ms":null},"read_only":true}"#,
            r#"{"type":"readable_directory","handle":1,"metadata":{"kind":"directory","size":"0","modified_ms":null},"read_only":true}"#,
            r#"{"type":"readable_entries","entries":[{"name":"owned","kind":"file","read_only":true}],"done":true,"skipped":0}"#,
        ] {
            let result: Result = serde_json::from_str(reply).unwrap();
            assert_eq!(serde_json::to_string(&result).unwrap(), reply);
        }
        assert!(serde_json::from_str::<Operation>(
            r#"{"op":"open_read","path":"/owned","read_links":true}"#
        )
        .is_err());
        assert!(serde_json::from_str::<Operation>(
            r#"{"op":"open_readable_file","path":"/owned","allow_outside":true}"#
        )
        .is_err());
        for invalid in [
            r#"{"type":"entries","entries":[{"name":"owned","kind":"file","read_only":true}],"done":true,"skipped":0}"#,
            r#"{"type":"readable_entries","entries":[{"name":"owned","kind":"file"}],"done":true,"skipped":0}"#,
            r#"{"type":"readable_entries","entries":[{"name":"owned","kind":"file","read_only":true,"target":"outside"}],"done":true,"skipped":0}"#,
        ] {
            assert!(serde_json::from_str::<Result>(invalid).is_err());
        }
    }

    #[test]
    fn host_files_opening_is_strict_and_keeps_legacy_messages_separate() {
        let scope = HostScope {
            host: Default::default(),
            installation: Default::default(),
            epoch: "29".into(),
        };
        let new = serde_json::to_value(HostOpen {
            version: HOST_VERSION,
            scope: scope.clone(),
        })
        .unwrap();
        assert!(serde_json::from_value::<Open>(new.clone()).is_err());
        let legacy = serde_json::to_value(Open {
            version: VERSION,
            target: Target {
                name: "legacy".into(),
                pid: 1,
                generation: "1".into(),
            },
        })
        .unwrap();
        assert_eq!(legacy.as_object().unwrap().len(), 2);
        assert!(serde_json::from_value::<HostOpen>(legacy).is_err());
        for epoch in ["0", "01", "-1", "18446744073709551616"] {
            let mut invalid = scope.clone();
            invalid.epoch = epoch.into();
            assert!(invalid.validate().is_err());
        }
        let policy = Policy {
            start_in: None,
            locations: None,
            delete: false,
            root: std::env::temp_dir().join("canary-private-root"),
            write: false,
        };
        assert!(!format!("{policy:?}").contains("canary-private-root"));
        let mut unknown = serde_json::to_value(policy).unwrap();
        unknown["future"] = true.into();
        assert!(serde_json::from_value::<Policy>(unknown).is_err());
        assert!(crate::protocol::validate_length(HOST_DESCRIPTION, 4096).is_ok());
        assert!(crate::protocol::validate_length(HOST_DESCRIPTION, 4097).is_err());
    }

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
    #[test]
    fn reviewed_delete_consent_defaults_off_and_preserves_old_serialized_policy() {
        let root = std::env::temp_dir().join("reviewed-delete-canary");
        let legacy = serde_json::json!({"root": root, "write": true});
        let decoded: Policy = serde_json::from_value(legacy.clone()).unwrap();
        assert!(!decoded.delete);
        assert_eq!(serde_json::to_value(&decoded).unwrap(), legacy);
        let mut approved = decoded;
        approved.delete = true;
        approved.validate().unwrap();
        approved.write = false;
        assert!(approved.validate().is_err());
        let mut unknown = serde_json::to_value(approved).unwrap();
        unknown["operations"] = serde_json::json!(["delete"]);
        assert!(serde_json::from_value::<Policy>(unknown).is_err());
        assert_eq!(
            serde_json::to_string(&Result::Ready {
                capabilities: Capabilities {
                    version: 1,
                    write: true,
                    chunk_limit: CHUNK_LIMIT,
                    page_limit: PAGE_LIMIT,
                    handle_limit: HANDLE_LIMIT,
                }
            })
            .unwrap(),
            r#"{"type":"ready","capabilities":{"version":1,"write":true,"chunk_limit":65536,"page_limit":64,"handle_limit":8}}"#
        );
    }
}
