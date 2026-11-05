use crate::{
    schema, ControllerId, Operation, Refusal, MAX_DIRECT_INPUT_BYTES, MAX_DIRECT_INPUT_KEYS,
    MAX_DIRECT_INPUT_KEY_BYTES,
};
use serde_json::{json, Map, Value};

const SEND_TEXT: &str = "festerm_terminal_send_text";
const SEND_KEYS: &str = "festerm_terminal_send_keys";

fn input_schema(text: bool) -> Value {
    let mut properties = Map::new();
    properties.insert(
        "enrollment_ref".into(),
        json!({
            "type": "string", "minLength": 32, "maxLength": 32,
            "pattern": "^[0-9a-f]{32}$",
            "description": "Connection-local association identifier, not a grant."
        }),
    );
    properties.insert(
        "presentation_revision".into(),
        json!({
            "type": "string", "minLength": 48, "maxLength": 48,
            "pattern": "^[0-9a-f]{48}$",
            "description": "Require this host-confirmed presentation revision; it is not authority."
        }),
    );
    let required = if text {
        properties.insert(
            "text".into(),
            json!({
                "type": "string", "minLength": 1, "maxLength": MAX_DIRECT_INPUT_BYTES,
                "description": "At most 16384 UTF-8 bytes before terminal encoding. Text may execute even without submit."
            }),
        );
        properties.insert(
            "submit".into(),
            json!({"type": "boolean", "default": false}),
        );
        vec!["enrollment_ref", "text"]
    } else {
        properties.insert(
            "keys".into(),
            json!({
                "type": "array", "minItems": 1, "maxItems": MAX_DIRECT_INPUT_KEYS,
                "items": {
                    "type": "string", "minLength": 1, "maxLength": MAX_DIRECT_INPUT_KEY_BYTES,
                    "description": "ASCII key token validated by the terminal encoder."
                }
            }),
        );
        vec!["enrollment_ref", "keys"]
    };
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn output_schema() -> Value {
    let codes: Vec<_> = Refusal::ALL.into_iter().map(Refusal::code).collect();
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "description": "Refused means definitely not admitted. Admitted is queue admission, not command completion. Unknown requires a client-owned interlock and must never be replayed automatically.",
        "type": "object",
        "oneOf": [
            {
                "properties": {
                    "status": {"const": "ok"},
                    "data": {
                        "type": "object",
                        "properties": {"disposition": {"const": "admitted"}},
                        "required": ["disposition"],
                        "additionalProperties": false
                    }
                },
                "required": ["status", "data"],
                "additionalProperties": false
            },
            {
                "properties": {
                    "status": {"const": "unknown"},
                    "data": {
                        "type": "object",
                        "properties": {"disposition": {"const": "outcome_unknown"}},
                        "required": ["disposition"],
                        "additionalProperties": false
                    }
                },
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
    [
        (SEND_TEXT, true, "Send explicitly approved text to one enrolled local terminal."),
        (SEND_KEYS, false, "Send explicitly approved keys to one enrolled local terminal."),
    ]
    .into_iter()
    .map(|(name, text, description)| {
        json!({
            "name": name,
            "description": format!("{description} Requires native handoff and a qualified backend; never automatically replay an unknown outcome."),
            "inputSchema": input_schema(text),
            "outputSchema": output_schema(),
            "annotations": {
                "readOnlyHint": false,
                "destructiveHint": true,
                "openWorldHint": true,
                "idempotentHint": false
            }
        })
    })
    .collect()
}

pub(crate) fn call(params: Option<&Value>) -> Option<Result<Operation, schema::CallError>> {
    let object = params?.as_object()?;
    let name = object.get("name")?.as_str()?;
    let text = match name {
        SEND_TEXT => true,
        SEND_KEYS => false,
        _ => return None,
    };
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
    Some(parse(arguments, text).ok_or(schema::CallError::Arguments))
}

fn parse(arguments: &Map<String, Value>, text: bool) -> Option<Operation> {
    let keys = if text {
        &["enrollment_ref", "text", "submit", "presentation_revision"][..]
    } else {
        &["enrollment_ref", "keys", "presentation_revision"][..]
    };
    if !schema::only_keys(arguments, keys) {
        return None;
    }
    let enrollment_ref = arguments.get("enrollment_ref")?.as_str()?;
    ControllerId::parse(enrollment_ref)?;
    let presentation_revision = match arguments.get("presentation_revision") {
        None => None,
        Some(value) => {
            let revision = value.as_str()?;
            if revision.len() != 48
                || !revision
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            {
                return None;
            }
            Some(revision.to_owned())
        }
    };
    if text {
        let value = arguments.get("text")?.as_str()?;
        if value.is_empty() || value.len() > MAX_DIRECT_INPUT_BYTES {
            return None;
        }
        let submit = match arguments.get("submit") {
            None => false,
            Some(value) => value.as_bool()?,
        };
        Some(Operation::TerminalSendText {
            enrollment_ref: enrollment_ref.to_owned(),
            text: value.to_owned(),
            submit,
            presentation_revision,
        })
    } else {
        let values = arguments.get("keys")?.as_array()?;
        if values.is_empty() || values.len() > MAX_DIRECT_INPUT_KEYS {
            return None;
        }
        let mut keys = Vec::with_capacity(values.len());
        for value in values {
            let key = value.as_str()?;
            if key.is_empty() || key.len() > MAX_DIRECT_INPUT_KEY_BYTES || !key.is_ascii() {
                return None;
            }
            keys.push(key.to_owned());
        }
        Some(Operation::TerminalSendKeys {
            enrollment_ref: enrollment_ref.to_owned(),
            keys,
            presentation_revision,
        })
    }
}
