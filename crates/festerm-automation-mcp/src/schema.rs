use crate::{
    ControllerId, Operation, Refusal, RequestId, MAX_CONTROLLER_LOCAL_ID_BYTES, MAX_READ_BYTES,
    MAX_REFERENCE_CHARS, MAX_WAIT_MS, VERIFICATION_CODE_LEN,
};
use serde_json::{json, Map, Value};

pub(crate) fn only_keys(object: &Map<String, Value>, keys: &[&str]) -> bool {
    object.keys().all(|key| keys.contains(&key.as_str()))
}

fn meta(object: &Map<String, Value>) -> bool {
    object.get("_meta").is_none_or(Value::is_object)
}

pub(crate) fn empty_params(params: Option<&Value>) -> bool {
    params.is_none_or(|value| {
        value
            .as_object()
            .is_some_and(|o| only_keys(o, &["_meta"]) && meta(o))
    })
}

pub(crate) fn initialize(params: Option<&Value>) -> bool {
    let Some(o) = params.and_then(Value::as_object) else {
        return false;
    };
    if !only_keys(
        o,
        &["protocolVersion", "capabilities", "clientInfo", "_meta"],
    ) || !meta(o)
        || !o
            .get("protocolVersion")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
        || !o.get("capabilities").is_some_and(Value::is_object)
    {
        return false;
    }
    let Some(info) = o.get("clientInfo").and_then(Value::as_object) else {
        return false;
    };
    if !only_keys(
        info,
        &[
            "name",
            "version",
            "title",
            "description",
            "icons",
            "websiteUrl",
        ],
    ) || !["name", "version"]
        .iter()
        .all(|key| info.get(*key).is_some_and(Value::is_string))
        || !["title", "description", "websiteUrl"]
            .iter()
            .all(|key| info.get(*key).is_none_or(Value::is_string))
    {
        return false;
    }
    info.get("icons").is_none_or(|icons| {
        icons.as_array().is_some_and(|icons| {
            icons.iter().all(|icon| {
                icon.as_object().is_some_and(|icon| {
                    only_keys(icon, &["src", "mimeType", "sizes", "theme"])
                        && icon.get("src").is_some_and(Value::is_string)
                        && icon.get("mimeType").is_none_or(Value::is_string)
                        && icon.get("sizes").is_none_or(|sizes| {
                            sizes
                                .as_array()
                                .is_some_and(|sizes| sizes.iter().all(Value::is_string))
                        })
                        && icon
                            .get("theme")
                            .is_none_or(|theme| matches!(theme.as_str(), Some("light" | "dark")))
                })
            })
        })
    })
}

pub(crate) fn cancelled(params: Option<&Value>) -> Option<RequestId> {
    let o = params?.as_object()?;
    if !only_keys(o, &["requestId", "reason", "_meta"])
        || !meta(o)
        || !o.get("reason").is_none_or(Value::is_string)
    {
        return None;
    }
    RequestId::parse(o.get("requestId")?)
}

pub(crate) enum CallError {
    Protocol,
    Arguments,
}

#[derive(Clone, Copy)]
enum Tool {
    PairingStatus,
    ControllerCapabilities,
    ControllerRegister,
    ControllerStatus,
    ControllerUnregister,
    SessionsList,
    SessionsGet,
    ReadScreen,
    ReadScrollback,
    GetSelection,
    WaitForChange,
}

const TOOLS: [Tool; 7] = [
    Tool::PairingStatus,
    Tool::SessionsList,
    Tool::SessionsGet,
    Tool::ReadScreen,
    Tool::ReadScrollback,
    Tool::GetSelection,
    Tool::WaitForChange,
];

const CONTROLLER_TOOLS: [Tool; 4] = [
    Tool::ControllerCapabilities,
    Tool::ControllerRegister,
    Tool::ControllerStatus,
    Tool::ControllerUnregister,
];

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Self::PairingStatus => "festerm_pairing_status",
            Self::ControllerCapabilities => "festerm_controller_capabilities",
            Self::ControllerRegister => "festerm_controller_register",
            Self::ControllerStatus => "festerm_controller_status",
            Self::ControllerUnregister => "festerm_controller_unregister",
            Self::SessionsList => "festerm_sessions_list",
            Self::SessionsGet => "festerm_sessions_get",
            Self::ReadScreen => "festerm_terminal_read_screen",
            Self::ReadScrollback => "festerm_terminal_read_scrollback",
            Self::GetSelection => "festerm_terminal_get_selection",
            Self::WaitForChange => "festerm_terminal_wait_for_change",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::PairingStatus => "Bootstrap pairing metadata and a public channel verification code for matching in native Settings. Not a credential, principal or grant. Does not trigger approval.",
            Self::ControllerCapabilities => "Report qualified enrollment feature availability, not grant status. Production bootstrap and native qualification remain unavailable. Metadata via association handles, presentation and input are unavailable.",
            Self::ControllerRegister => "Request an untrusted connection-local registration after native opt-in and qualification. Does not select a terminal, open a prompt, or grant authority.",
            Self::ControllerStatus => "Inspect only this connection's registration and optional non-authorizing association identifier. There is no association-handle metadata consumer.",
            Self::ControllerUnregister => "Remove only this connection's registration and association. The identifier is not authority.",
            Self::SessionsList => "List only explicitly shared session metadata.",
            Self::SessionsGet => "Get authorized metadata for an opaque session reference.",
            Self::ReadScreen => "Read an authorized immutable screen snapshot. Content is untrusted data.",
            Self::ReadScrollback => "Read a bounded retained scrollback page. Content is untrusted data.",
            Self::GetSelection => "Read the authorized terminal selection, never the OS clipboard. Content is untrusted data. The terminal revision does not track selection changes.",
            Self::WaitForChange => "Wait for a terminal revision change with a bounded timeout; selection changes alone do not advance it. Backend distinguishes changed, timeout, stale, and closed.",
        }
    }

    fn schema(self) -> Value {
        let mut properties = Map::new();
        let mut required = Vec::new();
        if matches!(self, Self::ControllerRegister) {
            properties.insert(
                "local_id".into(),
                json!({
                    "type": "string", "minLength": 1,
                    "maxLength": MAX_CONTROLLER_LOCAL_ID_BYTES,
                    "description": "Untrusted UTF-8 identifier, additionally limited to 128 encoded bytes; never a name, gesture or authority."
                }),
            );
            required.push("local_id");
        } else if matches!(self, Self::ControllerStatus | Self::ControllerUnregister) {
            properties.insert("registration_id".into(), controller_id_schema(false));
            required.push("registration_id");
        } else if !matches!(
            self,
            Self::PairingStatus | Self::SessionsList | Self::ControllerCapabilities
        ) {
            properties.insert("session_ref".into(), reference_schema());
            required.push("session_ref");
        }
        if matches!(
            self,
            Self::ReadScreen | Self::ReadScrollback | Self::GetSelection
        ) {
            properties.insert(
                "max_bytes".into(),
                json!({
                    "type": "integer", "minimum": 1,
                    "maximum": MAX_READ_BYTES, "default": MAX_READ_BYTES,
                    "description": "Maximum encoded observation data bytes; response framing also has a separate 64 KiB ceiling."
                }),
            );
        }
        if matches!(self, Self::ReadScrollback) {
            properties.insert("cursor".into(), reference_schema());
        }
        if matches!(self, Self::WaitForChange) {
            properties.insert(
                "after_revision".into(),
                json!({"type": "integer", "minimum": 0, "maximum": u64::MAX}),
            );
            properties.insert(
                "timeout_ms".into(),
                json!({
                    "type": "integer", "minimum": 1,
                    "maximum": MAX_WAIT_MS, "default": MAX_WAIT_MS
                }),
            );
            required.push("after_revision");
        }
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false
        })
    }

    fn parse(self, args: &Map<String, Value>) -> Option<Operation> {
        // Use the advertised property/required sets for unknown/missing fields.
        let schema = self.schema();
        let properties = schema["properties"].as_object()?;
        if !args.keys().all(|key| properties.contains_key(key))
            || !schema["required"]
                .as_array()?
                .iter()
                .all(|key| args.contains_key(key.as_str().unwrap_or_default()))
        {
            return None;
        }
        let target = || reference(args.get("session_ref")?);
        let read_limit = || bounded_u32(args.get("max_bytes"), MAX_READ_BYTES);
        Some(match self {
            Self::PairingStatus => Operation::PairingStatus,
            Self::ControllerCapabilities => Operation::ControllerCapabilities,
            Self::ControllerRegister => {
                let local_id = args.get("local_id")?.as_str()?;
                if local_id.is_empty() || local_id.len() > MAX_CONTROLLER_LOCAL_ID_BYTES {
                    return None;
                }
                Operation::ControllerRegister {
                    local_id: local_id.to_owned(),
                }
            }
            Self::ControllerStatus => Operation::ControllerStatus {
                registration_id: ControllerId::parse(args.get("registration_id")?.as_str()?)?,
            },
            Self::ControllerUnregister => Operation::ControllerUnregister {
                registration_id: ControllerId::parse(args.get("registration_id")?.as_str()?)?,
            },
            Self::SessionsList => Operation::SessionsList,
            Self::SessionsGet => Operation::SessionsGet {
                session_ref: target()?,
            },
            Self::ReadScreen => Operation::TerminalReadScreen {
                session_ref: target()?,
                max_bytes: read_limit()?,
            },
            Self::ReadScrollback => Operation::TerminalReadScrollback {
                session_ref: target()?,
                cursor: match args.get("cursor") {
                    None => None,
                    Some(value) => Some(reference(value)?),
                },
                max_bytes: read_limit()?,
            },
            Self::GetSelection => Operation::TerminalGetSelection {
                session_ref: target()?,
                max_bytes: read_limit()?,
            },
            Self::WaitForChange => Operation::TerminalWaitForChange {
                session_ref: target()?,
                after_revision: args.get("after_revision")?.as_u64()?,
                timeout_ms: bounded_u32(args.get("timeout_ms"), MAX_WAIT_MS)?,
            },
        })
    }

    fn output_schema(self) -> Value {
        let data = match self {
            Self::PairingStatus => json!({
                "type": "object",
                "properties": {
                    "paired": {"type": "boolean"},
                    "observation_enabled": {"type": "boolean"},
                    "verification_code": {
                        "type": "string",
                        "minLength": VERIFICATION_CODE_LEN,
                        "maxLength": VERIFICATION_CODE_LEN,
                        "pattern": "^[0-9a-f]{16}$",
                        "description": "Parent-generated public fingerprint for this exact private channel. Not a secret, principal, credential or grant; useless on another stream."
                    }
                },
                "required": ["paired", "observation_enabled", "verification_code"],
                "additionalProperties": false
            }),
            Self::ControllerCapabilities => json!({
                "type": "object",
                "properties": {
                    "enrollment_available": {
                        "type": "boolean",
                        "description": "Backend-qualified enrollment feature availability, not authorization or grant status."
                    },
                    "metadata_available": {
                        "type": "boolean", "const": false,
                        "description": "Availability of metadata access via an association handle, not grant status. Always false until the association-handle metadata consumer is implemented."
                    },
                    "metadata_requires_separate_grant": {
                        "type": "boolean", "const": true,
                        "description": "Future metadata access requires a separate native-authorized discover grant; this does not assert a grant exists."
                    },
                    "presentation_available": {"type": "boolean", "const": false},
                    "input_available": {"type": "boolean", "const": false}
                },
                "required": [
                    "enrollment_available", "metadata_available", "metadata_requires_separate_grant",
                    "presentation_available", "input_available"
                ],
                "additionalProperties": false
            }),
            Self::ControllerRegister | Self::ControllerStatus => json!({
                "type": "object",
                "properties": {
                    "registration_id": controller_id_schema(false),
                    "association_id": controller_id_schema(true)
                },
                "required": ["registration_id", "association_id"],
                "additionalProperties": false
            }),
            Self::ControllerUnregister => json!({
                "type": "object",
                "properties": {
                    "registration_id": controller_id_schema(false),
                    "removed": {"type": "boolean", "const": true}
                },
                "required": ["registration_id", "removed"],
                "additionalProperties": false
            }),
            Self::SessionsList | Self::SessionsGet => {
                // P1 MCP approval caps shared targets at 32; get filters to one.
                let max_sessions = if matches!(self, Self::SessionsGet) {
                    1
                } else {
                    32
                };
                json!({
                    "type": "object",
                    "properties": {
                        "sessions": {
                            "type": "array",
                            "maxItems": max_sessions,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "session_ref": reference_schema(),
                                    "label": {"type": "string", "maxLength": 128},
                                    "lifecycle": {"type": "string", "const": "running"},
                                    "untrusted": {"type": "boolean", "const": true}
                                },
                                "required": ["session_ref", "label", "lifecycle", "untrusted"],
                                "additionalProperties": false
                            },
                        },
                        "untrusted": {"type": "boolean", "const": true}
                    },
                    "required": ["sessions", "untrusted"],
                    "additionalProperties": false
                })
            }
            Self::ReadScreen => json!({
                "type": "object",
                "properties": {
                    "revision": unsigned_schema(),
                    "screen_generation": unsigned_schema(),
                    "columns": {"type": "integer", "minimum": 2, "maximum": u64::MAX},
                    "rows": {"type": "integer", "minimum": 1, "maximum": u64::MAX},
                    "cursor": {
                        "type": "object",
                        "properties": {
                            "column": unsigned_schema(),
                            "row": unsigned_schema()
                        },
                        "required": ["column", "row"],
                        "additionalProperties": false
                    },
                    "alternate_screen": {"type": "boolean"},
                    "text": {"type": "string"},
                    "truncated": {"type": "boolean"},
                    "untrusted": {"type": "boolean", "const": true}
                },
                "required": [
                    "revision", "screen_generation", "columns", "rows", "cursor",
                    "alternate_screen", "text", "truncated", "untrusted"
                ],
                "additionalProperties": false
            }),
            Self::ReadScrollback => json!({
                "type": "object",
                "properties": {
                    "revision": unsigned_schema(),
                    "text": {"type": "string"},
                    "gap": {
                        "type": "boolean",
                        "description": "True when the cursor belongs to a different terminal revision."
                    },
                    "next_cursor": {
                        "type": ["string", "null"],
                        "minLength": 1,
                        "maxLength": MAX_REFERENCE_CHARS,
                        "description": "Opaque continuation for a truncated page; null otherwise."
                    },
                    "truncated": {"type": "boolean"},
                    "untrusted": {"type": "boolean", "const": true},
                    "retained_primary_history_only": {"type": "boolean", "const": true},
                    "evicted_lines": unsigned_schema(),
                    "oversize_lines": unsigned_schema()
                },
                "required": ["revision", "text", "gap", "next_cursor", "truncated", "untrusted"],
                "additionalProperties": false,
                "oneOf": [
                    {
                        "properties": {
                            "revision": {},
                            "text": {"const": ""},
                            "gap": {"const": true},
                            "next_cursor": {"type": "null"},
                            "truncated": {"const": false},
                            "untrusted": {}
                        },
                        "additionalProperties": false
                    },
                    {
                        "properties": {"gap": {"const": false}},
                        "required": [
                            "retained_primary_history_only", "evicted_lines", "oversize_lines"
                        ],
                        "oneOf": [
                            {
                                "properties": {
                                    "truncated": {"const": true},
                                    "next_cursor": {"type": "string"}
                                }
                            },
                            {
                                "properties": {
                                    "truncated": {"const": false},
                                    "next_cursor": {"type": "null"}
                                }
                            }
                        ]
                    }
                ]
            }),
            Self::GetSelection => json!({
                "type": "object",
                "properties": {
                    "revision": unsigned_schema(),
                    "present": {"type": "boolean"},
                    "text": {"type": ["string", "null"]},
                    "truncated": {"type": "boolean", "const": false},
                    "untrusted": {"type": "boolean", "const": true}
                },
                "required": ["revision", "present", "text", "truncated", "untrusted"],
                "additionalProperties": false,
                "oneOf": [
                    {"properties": {"present": {"const": true}, "text": {"type": "string"}}},
                    {"properties": {"present": {"const": false}, "text": {"type": "null"}}}
                ]
            }),
            Self::WaitForChange => json!({
                "type": "object",
                "properties": {
                    "revision": unsigned_schema(),
                    "changed": {"type": "boolean"},
                    "timed_out": {"type": "boolean"},
                    "untrusted": {"type": "boolean", "const": true}
                },
                "required": ["revision", "changed", "timed_out", "untrusted"],
                "additionalProperties": false,
                "oneOf": [
                    {"properties": {"changed": {"const": true}, "timed_out": {"const": false}}},
                    {"properties": {"changed": {"const": false}, "timed_out": {"const": true}}}
                ]
            }),
        };
        let codes: Vec<_> = Refusal::ALL.into_iter().map(Refusal::code).collect();
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "oneOf": [
                {
                    "properties": {"status": {"const": "ok"}, "data": data},
                    "required": ["status", "data"],
                    "additionalProperties": false
                },
                {
                    "properties": {
                        "status": {"const": "refused"},
                        "code": {"type": "string", "enum": codes}
                    },
                    "required": ["status", "code"],
                    "additionalProperties": false
                }
            ]
        })
    }
}

fn unsigned_schema() -> Value {
    json!({"type": "integer", "minimum": 0, "maximum": u64::MAX})
}

fn controller_id_schema(nullable: bool) -> Value {
    let kind = if nullable {
        json!(["string", "null"])
    } else {
        json!("string")
    };
    json!({
        "type": kind, "minLength": 32, "maxLength": 32,
        "pattern": "^[0-9a-f]{32}$",
        "description": "Connection-local opaque correlation identifier, not authority."
    })
}

fn reference_schema() -> Value {
    json!({
        "type": "string", "minLength": 1, "maxLength": MAX_REFERENCE_CHARS,
        "description": "Opaque backend-validated reference, not authority."
    })
}

fn reference(value: &Value) -> Option<String> {
    let value = value.as_str()?;
    if value.is_empty() || value.chars().count() > MAX_REFERENCE_CHARS {
        return None;
    }
    Some(value.to_owned())
}

fn bounded_u32(value: Option<&Value>, max: u32) -> Option<u32> {
    match value {
        None => Some(max),
        Some(value) => {
            let value = value.as_u64()?;
            (value >= 1 && value <= u64::from(max)).then_some(value as u32)
        }
    }
}

pub(crate) fn call(
    params: Option<&Value>,
    controller_enrollment: bool,
) -> Result<Operation, CallError> {
    let o = params
        .and_then(Value::as_object)
        .ok_or(CallError::Protocol)?;
    if !only_keys(o, &["name", "arguments", "_meta"]) || !meta(o) {
        return Err(CallError::Protocol);
    }
    let name = o
        .get("name")
        .and_then(Value::as_str)
        .ok_or(CallError::Protocol)?;
    let tool = TOOLS
        .iter()
        .chain(CONTROLLER_TOOLS.iter().filter(|_| controller_enrollment))
        .find(|tool| tool.name() == name)
        .ok_or(CallError::Protocol)?;
    let empty = Map::new();
    let args = match o.get("arguments") {
        None => &empty,
        Some(value) => value.as_object().ok_or(CallError::Protocol)?,
    };
    tool.parse(args).ok_or(CallError::Arguments)
}

pub(crate) fn tools(controller_enrollment: bool) -> Vec<Value> {
    TOOLS
        .iter()
        .chain(CONTROLLER_TOOLS.iter().filter(|_| controller_enrollment))
        .map(|tool| {
            json!({
                "name": tool.name(),
                "description": tool.description(),
                "inputSchema": tool.schema(),
                "outputSchema": tool.output_schema(),
                "annotations": {
                    "readOnlyHint": !matches!(tool, Tool::ControllerRegister | Tool::ControllerUnregister),
                    "destructiveHint": matches!(tool, Tool::ControllerUnregister),
                    "openWorldHint": false,
                    "idempotentHint": !matches!(tool, Tool::ControllerUnregister)
                }
            })
        })
        .collect()
}
