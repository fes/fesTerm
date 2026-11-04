use crate::{schema, MAX_REFERENCE_CHARS};
use serde_json::{json, Map, Value};

const CAPABILITIES: &str = "festerm_presentation_capabilities";
const PRESENT: &str = "festerm_workspace_present";
const MAX_PRESENTATION_WAIT_MS: u64 = 5_000;

pub(crate) enum Call {
    Capabilities,
    Present,
}

pub(crate) fn unavailable() -> Value {
    json!({
        "presentation": {
            "available": false,
            "reason": "NATIVE_EVIDENCE_UNQUALIFIED"
        },
        "backends": {
            "local_pty": "NATIVE_EVIDENCE_UNQUALIFIED",
            "other": "UNSUPPORTED_BACKEND"
        },
        "enrollment_available": false,
        "input_available": false
    })
}

fn reference_schema() -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "maxLength": MAX_REFERENCE_CHARS,
        "description": "Opaque, connection-bound reference; never a grant or native decision."
    })
}

fn output_schema(data: Value, codes: &[&str]) -> Value {
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

pub(crate) fn tools() -> Vec<Value> {
    let availability = json!({
        "type": "object",
        "oneOf": [
            {
                "properties": {
                    "available": {"const": true},
                    "reason": {"type": "null"}
                },
                "required": ["available", "reason"],
                "additionalProperties": false
            },
            {
                "properties": {
                    "available": {"const": false},
                    "reason": {
                        "enum": [
                            "NATIVE_EVIDENCE_UNQUALIFIED",
                            "UNSUPPORTED_VIEWPORT",
                            "UNSUPPORTED_BACKEND"
                        ]
                    }
                },
                "required": ["available", "reason"],
                "additionalProperties": false
            }
        ]
    });
    vec![
        json!({
            "name": CAPABILITIES,
            "description": "Content-free presentation, enrollment and input availability. Does not grant access or request approval.",
            "inputSchema": {
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object", "properties": {}, "required": [], "additionalProperties": false
            },
            "outputSchema": output_schema(json!({
                "type": "object",
                "properties": {
                    "presentation": availability,
                    "backends": {
                        "type": "object",
                        "properties": {
                            "local_pty": {
                                "enum": ["AVAILABLE", "NATIVE_EVIDENCE_UNQUALIFIED", "UNSUPPORTED_VIEWPORT"]
                            },
                            "other": {"const": "UNSUPPORTED_BACKEND"}
                        },
                        "required": ["local_pty", "other"],
                        "additionalProperties": false
                    },
                    "enrollment_available": {"type": "boolean"},
                    "input_available": {"type": "boolean"}
                },
                "required": ["presentation", "backends", "enrollment_available", "input_available"],
                "additionalProperties": false
            }), &["INVALID_ARGUMENTS", "UNAVAILABLE"]),
            "annotations": {
                "readOnlyHint": true, "destructiveHint": false,
                "openWorldHint": false, "idempotentHint": true
            }
        }),
        json!({
            "name": PRESENT,
            "description": "Request presentation of an exact enrollment. Unavailable unless separately granted and the native backend is qualified; queued focus is never confirmation.",
            "inputSchema": {
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "properties": {
                    "enrollment_ref": reference_schema(),
                    "timeout_ms": {
                        "type": "integer", "minimum": 1,
                        "maximum": MAX_PRESENTATION_WAIT_MS, "default": MAX_PRESENTATION_WAIT_MS
                    }
                },
                "required": ["enrollment_ref"],
                "additionalProperties": false
            },
            "outputSchema": output_schema(json!({
                "type": "object",
                "properties": {
                    "target_ref": reference_schema(),
                    "presentation_revision": {
                        "type": "string", "minLength": 48, "maxLength": 48,
                        "pattern": "^[0-9a-f]{48}$",
                        "description": "Opaque host-owned revision of actual selected tab/window and native foreground activation; not a grant."
                    }
                },
                "required": ["target_ref", "presentation_revision"],
                "additionalProperties": false
            }), &[
                "UNAVAILABLE", "NOT_AUTHORIZED", "STALE_TARGET", "CLOSED",
                "FOREGROUND_REFUSED", "TIMED_OUT", "STALE_PRESENTATION", "INVALID_ARGUMENTS"
            ]),
            "annotations": {
                "readOnlyHint": false, "destructiveHint": false,
                "openWorldHint": false, "idempotentHint": false
            }
        }),
    ]
}

pub(crate) fn call(params: Option<&Value>) -> Option<Result<Call, schema::CallError>> {
    let object = params?.as_object()?;
    let name = object.get("name")?.as_str()?;
    if name != CAPABILITIES && name != PRESENT {
        return None;
    }
    if !schema::only_keys(object, &["name", "arguments", "_meta"])
        || object.get("_meta").is_some_and(|meta| !meta.is_object())
    {
        return Some(Err(schema::CallError::Protocol));
    }
    let empty = Map::new();
    let arguments = match object.get("arguments") {
        None => &empty,
        Some(value) => match value.as_object() {
            Some(arguments) => arguments,
            None => return Some(Err(schema::CallError::Protocol)),
        },
    };
    if name == CAPABILITIES {
        return Some(if arguments.is_empty() {
            Ok(Call::Capabilities)
        } else {
            Err(schema::CallError::Arguments)
        });
    }
    let valid_reference = arguments
        .get("enrollment_ref")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty() && value.chars().count() <= MAX_REFERENCE_CHARS);
    let valid_timeout = arguments.get("timeout_ms").is_none_or(|value| {
        value
            .as_u64()
            .is_some_and(|value| (1..=MAX_PRESENTATION_WAIT_MS).contains(&value))
    });
    Some(
        if schema::only_keys(arguments, &["enrollment_ref", "timeout_ms"])
            && valid_reference
            && valid_timeout
        {
            Ok(Call::Present)
        } else {
            Err(schema::CallError::Arguments)
        },
    )
}
