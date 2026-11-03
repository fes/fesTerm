use crate::{snapshot::Parameters, Error, MAX_RUN_MS};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::io::{Read, Write};

pub const MAX_FRAME_BYTES: usize = 65_536;
pub const CHUNK_BYTES: usize = 8_192;
pub const MAX_PENDING_CALLS: usize = 64;
pub const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Limits {
    pub run_ms: u64,
    /// Strictly a lower quota than the fixed 5s native hard ceiling.
    pub cpu_ms: u64,
    pub max_host_calls: u16,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            run_ms: MAX_RUN_MS,
            cpu_ms: 5_000,
            max_host_calls: 1_024,
        }
    }
}

impl Limits {
    pub fn validate(&self) -> Result<(), Error> {
        if self.run_ms == 0
            || self.run_ms > MAX_RUN_MS
            || self.cpu_ms == 0
            || self.cpu_ms > 5_000
            || self.max_host_calls == 0
            || self.max_host_calls > 1_024
        {
            return Err(Error::Protocol);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub id: String,
    pub generation: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "method",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum HostRequest {
    Observe {},
    ReadScreen {
        target: Target,
        max_bytes: u16,
    },
    ReadScrollback {
        target: Target,
        max_bytes: u16,
        max_lines: u16,
    },
    GetSelection {
        target: Target,
        max_bytes: u16,
    },
    WaitForChange {
        target: Target,
        after_revision: u64,
        timeout_ms: u32,
    },
    SendText {
        target: Target,
        text: String,
    },
    SendKeys {
        target: Target,
        keys: Vec<String>,
    },
    Open {
        profile_id: String,
        revision: String,
    },
    InvokeWorkflow {
        digest: String,
        #[serde(deserialize_with = "crate::snapshot::unique_map")]
        parameters: Parameters,
    },
}

impl HostRequest {
    pub fn method(&self) -> &'static str {
        match self {
            Self::Observe {} => "observe",
            Self::ReadScreen { .. } => "readScreen",
            Self::ReadScrollback { .. } => "readScrollback",
            Self::GetSelection { .. } => "getSelection",
            Self::WaitForChange { .. } => "waitForChange",
            Self::SendText { .. } => "sendText",
            Self::SendKeys { .. } => "sendKeys",
            Self::Open { .. } => "open",
            Self::InvokeWorkflow { .. } => "invokeWorkflow",
        }
    }

    pub fn validate(&self) -> Result<(), Error> {
        use crate::snapshot::token;
        let valid_target = |t: &Target| token(&t.id) && t.generation <= MAX_SAFE_INTEGER;
        let valid = match self {
            Self::Observe {} => true,
            Self::ReadScreen { target, max_bytes } | Self::GetSelection { target, max_bytes } => {
                valid_target(target) && (1..=16_384).contains(max_bytes)
            }
            Self::ReadScrollback {
                target,
                max_bytes,
                max_lines,
            } => {
                valid_target(target)
                    && (1..=16_384).contains(max_bytes)
                    && (1..=1_000).contains(max_lines)
            }
            Self::WaitForChange {
                target,
                after_revision,
                timeout_ms,
            } => {
                valid_target(target)
                    && *after_revision <= MAX_SAFE_INTEGER
                    && (1..=30_000).contains(timeout_ms)
            }
            Self::SendText { target, text } => valid_target(target) && text.len() <= 16_384,
            Self::SendKeys { target, keys } => {
                valid_target(target)
                    && !keys.is_empty()
                    && keys.len() <= 64
                    && keys.iter().all(|key| token(key) && key.len() <= 64)
            }
            Self::Open {
                profile_id,
                revision,
            } => token(profile_id) && token(revision),
            Self::InvokeWorkflow { digest, parameters } => {
                digest.len() == 64
                    && digest.bytes().all(|b| b.is_ascii_hexdigit())
                    && parameters.len() <= 64
                    && serde_json::to_vec(parameters)
                        .map_err(|_| Error::Protocol)?
                        .len()
                        <= 32_768
            }
        };
        if valid {
            Ok(())
        } else {
            Err(Error::Protocol)
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum HostError {
    Denied,
    Revoked,
    StaleTarget,
    Budget,
    Cancelled,
    Expired,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TargetInfo {
    pub target: Target,
    pub revision: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum HostResult {
    Observation {
        targets: Vec<TargetInfo>,
    },
    Text {
        text: String,
        revision: u64,
        truncated: bool,
    },
    Change {
        revision: u64,
        changed: bool,
    },
    Accepted {},
    Opened {
        target: Target,
    },
    Workflow {
        value: serde_json::Value,
    },
    Error {
        code: HostError,
    },
}

impl HostResult {
    pub fn validate_for(&self, request: &HostRequest) -> Result<(), Error> {
        let matches = match (self, request) {
            (Self::Error { .. }, _) => true,
            (Self::Observation { targets }, HostRequest::Observe {}) => {
                targets.len() <= 64
                    && targets.iter().all(|t| {
                        crate::snapshot::token(&t.target.id)
                            && t.target.generation <= MAX_SAFE_INTEGER
                            && t.revision <= MAX_SAFE_INTEGER
                    })
            }
            (
                Self::Text { text, revision, .. },
                HostRequest::ReadScreen { max_bytes, .. }
                | HostRequest::ReadScrollback { max_bytes, .. }
                | HostRequest::GetSelection { max_bytes, .. },
            ) => text.len() <= usize::from(*max_bytes) && *revision <= MAX_SAFE_INTEGER,
            (Self::Change { revision, .. }, HostRequest::WaitForChange { .. }) => {
                *revision <= MAX_SAFE_INTEGER
            }
            (Self::Accepted {}, HostRequest::SendText { .. } | HostRequest::SendKeys { .. }) => {
                true
            }
            (Self::Opened { target }, HostRequest::Open { .. }) => {
                crate::snapshot::token(&target.id) && target.generation <= MAX_SAFE_INTEGER
            }
            (Self::Workflow { value }, HostRequest::InvokeWorkflow { .. }) => {
                validate_value(value).is_ok()
            }
            _ => false,
        };
        if !matches || serde_json::to_vec(self).map_err(|_| Error::Protocol)?.len() > 49_152 {
            return Err(Error::Protocol);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ParentMessage {
    Begin {
        digest: String,
        snapshot_bytes: u32,
        #[serde(deserialize_with = "crate::snapshot::unique_map")]
        parameters: Parameters,
        limits: Limits,
    },
    SourceChunk {
        bytes: Vec<u8>,
    },
    Run {},
    HostResult {
        id: u32,
        result: HostResult,
    },
    Cancel {},
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "camelCase", deny_unknown_fields)]
pub enum Outcome {
    /// Script-controlled data; only release through the broker's authorized result channel.
    Completed {
        value: serde_json::Value,
    },
    ScriptFailed {},
    ResourceLimit {},
    UnsettledWork {},
    Cancelled {},
    ProtocolFailed {},
    Unavailable {},
}

impl std::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Completed { .. } => "Completed(<authorized-result>)",
            Self::ScriptFailed {} => "ScriptFailed",
            Self::ResourceLimit {} => "ResourceLimit",
            Self::UnsettledWork {} => "UnsettledWork",
            Self::Cancelled {} => "Cancelled",
            Self::ProtocolFailed {} => "ProtocolFailed",
            Self::Unavailable {} => "Unavailable",
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum WorkerMessage {
    Ready { profile: String },
    HostCall { id: u32, request: HostRequest },
    Finished { outcome: Outcome },
    Failed { code: WorkerFailure },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkerFailure {
    Unavailable,
    InvalidSnapshot,
    Protocol,
    Io,
    Deadline,
}

/// Bounded structured result values, checked without recursive traversal.
pub fn validate_value(value: &serde_json::Value) -> Result<(), Error> {
    let mut pending = vec![(value, 0)];
    let mut nodes = 0;
    let mut bytes = 0;
    while let Some((value, depth)) = pending.pop() {
        nodes += 1;
        if depth > 16 || nodes > 4_096 || bytes > 49_152 {
            return Err(Error::Protocol);
        }
        match value {
            serde_json::Value::String(s) => {
                if s.len() > 16_384 {
                    return Err(Error::Protocol);
                }
                bytes += s.len();
            }
            serde_json::Value::Array(a) => {
                if a.len() > 256 {
                    return Err(Error::Protocol);
                }
                pending.extend(a.iter().map(|v| (v, depth + 1)));
            }
            serde_json::Value::Object(o) => {
                if o.len() > 64 || o.keys().any(|k| k.len() > 256) {
                    return Err(Error::Protocol);
                }
                bytes += o.keys().map(String::len).sum::<usize>();
                pending.extend(o.values().map(|v| (v, depth + 1)));
            }
            _ => {}
        }
    }
    if bytes > 49_152 {
        return Err(Error::Protocol);
    }
    Ok(())
}

pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, Error> {
    let payload = serde_json::to_vec(value).map_err(|_| Error::Protocol)?;
    if payload.is_empty() || payload.len() > MAX_FRAME_BYTES {
        return Err(Error::Protocol);
    }
    let mut frame = Vec::with_capacity(payload.len() + 4);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<(), Error> {
    writer.write_all(&encode_frame(value)?)?;
    writer.flush()?;
    Ok(())
}

pub fn read_frame<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<T, Error> {
    let mut header = [0; 4];
    reader.read_exact(&mut header)?;
    let size = u32::from_be_bytes(header) as usize;
    if size == 0 || size > MAX_FRAME_BYTES {
        return Err(Error::Protocol);
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)
}

/// Bounded incremental decoder for nonblocking inherited pipes.
#[derive(Default)]
pub(crate) struct Decoder {
    bytes: Vec<u8>,
}

impl Decoder {
    pub(crate) fn read<T: DeserializeOwned>(
        &mut self,
        input: &mut impl Read,
    ) -> Result<Option<T>, Error> {
        loop {
            let need = if self.bytes.len() < 4 {
                4 - self.bytes.len()
            } else {
                let size = u32::from_be_bytes(self.bytes[..4].try_into().unwrap()) as usize;
                if size == 0 || size > MAX_FRAME_BYTES {
                    return Err(Error::Protocol);
                }
                let total = 4 + size;
                if self.bytes.len() == total {
                    let result =
                        serde_json::from_slice(&self.bytes[4..]).map_err(|_| Error::Protocol);
                    self.bytes.clear();
                    return result.map(Some);
                }
                total - self.bytes.len()
            };
            let mut chunk = [0; 4_096];
            let take = need.min(chunk.len());
            match input.read(&mut chunk[..take]) {
                Ok(0) => return Err(Error::Protocol),
                Ok(n) => self.bytes.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_strict_framing() {
        assert!(read_frame::<_, ParentMessage>(&mut &65_537_u32.to_be_bytes()[..]).is_err());
        assert!(read_frame::<_, ParentMessage>(&mut &0_u32.to_be_bytes()[..]).is_err());
        assert!(read_frame::<_, ParentMessage>(&mut &[0, 0, 0, 5, b'{'][..]).is_err());
        assert!(serde_json::from_str::<ParentMessage>(r#"{"type":"cancel","extra":1}"#).is_err());
        assert!(
            serde_json::from_str::<HostRequest>(r#"{"method":"observe","approve":true}"#).is_err()
        );
        let bytes = encode_frame(&ParentMessage::Cancel {}).unwrap();
        assert!(matches!(
            read_frame::<_, ParentMessage>(&mut &bytes[..]).unwrap(),
            ParentMessage::Cancel {}
        ));
    }

    #[test]
    fn typed_result_and_request_limits() {
        let request = HostRequest::ReadScreen {
            target: Target {
                id: "s1".into(),
                generation: 1,
            },
            max_bytes: 8,
        };
        assert!(HostResult::Accepted {}.validate_for(&request).is_err());
        assert!(HostResult::Text {
            text: "0123456789".into(),
            revision: 1,
            truncated: false
        }
        .validate_for(&request)
        .is_err());
    }
}
