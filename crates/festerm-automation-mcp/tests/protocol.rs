use festerm_automation_mcp::{
    read_bounded_line, BackendRequest, BackendResult, ControllerId, Event, Lifecycle, LineError,
    Machine, Operation, PairingStatus, Refusal, Response, VerificationCode, MAX_COLLECTION_ITEMS,
    MAX_CONTROLLER_LOCAL_ID_BYTES, MAX_DEPTH, MAX_ID_BYTES, MAX_IN_FLIGHT, MAX_MESSAGE_BYTES,
    MAX_READ_BYTES, MAX_STRING_BYTES, MAX_VALUE_NODES, MAX_WAIT_MS, PROTOCOL_VERSION,
};
use serde_json::{json, Value};
use std::{cmp::Ordering, collections::BTreeSet, io::Cursor};

fn request(id: Value, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params})
}

fn initialize(machine: &mut Machine, version: &str) -> Value {
    reply(machine.process_value(request(
        json!(1),
        "initialize",
        json!({
            "protocolVersion": version,
            "clientInfo": {"name": "untrusted client", "version": "0"},
            "capabilities": {}
        }),
    )))
}

fn ready() -> Machine {
    let mut machine = Machine::new();
    initialize(&mut machine, PROTOCOL_VERSION);
    assert!(matches!(
        machine.process_value(json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
        Event::NoReply
    ));
    machine
}

fn decode(response: Response) -> Value {
    assert!(response.as_bytes().len() <= MAX_MESSAGE_BYTES);
    assert!(response.as_bytes().ends_with(b"\n"));
    assert!(!response.as_bytes()[..response.as_bytes().len() - 1].contains(&b'\n'));
    serde_json::from_slice(response.as_bytes()).unwrap()
}

fn reply(event: Event) -> Value {
    match event {
        Event::Reply(response) => decode(response),
        _ => panic!("expected response"),
    }
}

fn backend(event: Event) -> BackendRequest {
    match event {
        Event::Backend(request) => request,
        _ => panic!("expected backend request"),
    }
}

fn call(id: Value, tool: &str, arguments: Value) -> Value {
    request(
        id,
        "tools/call",
        json!({"name": tool, "arguments": arguments}),
    )
}

fn error(event: Event, code: i32) -> Value {
    let value = reply(event);
    assert_eq!(value["error"]["code"], code);
    assert!(value.get("result").is_none());
    value
}

fn tool_error(event: Event, code: &str) -> Value {
    let value = reply(event);
    assert!(value.get("error").is_none());
    assert_eq!(value["result"]["isError"], true);
    assert_eq!(value["result"]["structuredContent"]["code"], code);
    value
}

fn numeric_cmp(left: &Value, right: &Value) -> Option<Ordering> {
    let integer = |number: &Value| {
        number
            .as_i64()
            .map(i128::from)
            .or_else(|| number.as_u64().map(i128::from))
    };
    match (integer(left), integer(right)) {
        (Some(left), Some(right)) => Some(left.cmp(&right)),
        _ => left.as_f64()?.partial_cmp(&right.as_f64()?),
    }
}

// Exercise the JSON Schema keywords used by the catalog; do not silently ignore new ones.
fn schema_matches(schema: &Value, value: &Value) -> bool {
    let schema = schema.as_object().unwrap();
    assert!(schema.keys().all(|key| matches!(
        key.as_str(),
        "$schema"
            | "description"
            | "type"
            | "properties"
            | "required"
            | "additionalProperties"
            | "items"
            | "oneOf"
            | "const"
            | "enum"
            | "minimum"
            | "maximum"
            | "minLength"
            | "maxLength"
            | "maxItems"
            | "pattern"
            | "default"
    )));
    let has_type = |name: &str| match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "integer" => {
            value.as_i64().is_some()
                || value.as_u64().is_some()
                || value
                    .as_f64()
                    .is_some_and(|number| number.is_finite() && number.fract() == 0.0)
        }
        _ => panic!("unsupported schema type {name}"),
    };
    if let Some(types) = schema.get("type") {
        let matches = match types {
            Value::String(name) => has_type(name),
            Value::Array(names) => names.iter().any(|name| has_type(name.as_str().unwrap())),
            _ => panic!("invalid schema type"),
        };
        if !matches {
            return false;
        }
    }
    if schema
        .get("const")
        .is_some_and(|expected| expected != value)
        || schema
            .get("enum")
            .is_some_and(|allowed| !allowed.as_array().unwrap().contains(value))
    {
        return false;
    }
    if schema
        .get("minimum")
        .is_some_and(|minimum| numeric_cmp(value, minimum) == Some(Ordering::Less))
        || schema
            .get("maximum")
            .is_some_and(|maximum| numeric_cmp(value, maximum) == Some(Ordering::Greater))
    {
        return false;
    }
    if let Some(string) = value.as_str() {
        let length = string.chars().count() as u64;
        if schema
            .get("minLength")
            .is_some_and(|minimum| length < minimum.as_u64().unwrap())
            || schema
                .get("maxLength")
                .is_some_and(|maximum| length > maximum.as_u64().unwrap())
        {
            return false;
        }
        if let Some(pattern) = schema.get("pattern") {
            let expected = match pattern.as_str().unwrap() {
                "^[0-9a-f]{16}$" => 16,
                "^[0-9a-f]{32}$" => 32,
                "^[0-9a-f]{48}$" => 48,
                other => panic!("unsupported schema pattern {other}"),
            };
            if string.len() != expected
                || !string
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return false;
            }
        }
    }
    if let Some(object) = value.as_object() {
        if schema.get("required").is_some_and(|required| {
            required
                .as_array()
                .unwrap()
                .iter()
                .any(|key| !object.contains_key(key.as_str().unwrap()))
        }) {
            return false;
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        for (key, member) in object {
            match properties.and_then(|properties| properties.get(key)) {
                Some(property) if !schema_matches(property, member) => return false,
                None if schema.get("additionalProperties") == Some(&json!(false)) => return false,
                _ => {}
            }
        }
    }
    if let Some(values) = value.as_array() {
        if schema
            .get("maxItems")
            .is_some_and(|maximum| values.len() as u64 > maximum.as_u64().unwrap())
            || schema
                .get("items")
                .is_some_and(|items| !values.iter().all(|item| schema_matches(items, item)))
        {
            return false;
        }
    }
    schema.get("oneOf").is_none_or(|alternatives| {
        alternatives
            .as_array()
            .unwrap()
            .iter()
            .filter(|alternative| schema_matches(alternative, value))
            .count()
            == 1
    })
}

#[test]
fn presentation_contract_advertises_refusal_without_changing_p1_or_starting_work() {
    let mut observation_only = ready();
    error(
        observation_only.process_value(call(
            json!(9),
            "festerm_workspace_present",
            json!({"enrollment_ref": "opaque"}),
        )),
        -32602,
    );

    let mut machine = Machine::with_unavailable_presentation();
    initialize(&mut machine, PROTOCOL_VERSION);
    assert!(matches!(
        machine.process_value(json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
        Event::NoReply
    ));
    let catalog = reply(machine.process_value(request(json!(2), "tools/list", json!({}))));
    let tools = catalog["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 9);
    let capability = tools
        .iter()
        .find(|tool| tool["name"] == "festerm_presentation_capabilities")
        .unwrap();
    let present = tools
        .iter()
        .find(|tool| tool["name"] == "festerm_workspace_present")
        .unwrap();
    assert_eq!(present["annotations"]["readOnlyHint"], false);
    assert_eq!(present["annotations"]["idempotentHint"], false);
    let status = reply(machine.process_value(call(
        json!(3),
        "festerm_presentation_capabilities",
        json!({}),
    )));
    let structured = &status["result"]["structuredContent"];
    assert!(schema_matches(&capability["outputSchema"], structured));
    assert_eq!(
        structured["data"],
        json!({
            "presentation": {"available": false, "reason": "NATIVE_EVIDENCE_UNQUALIFIED"},
            "backends": {
                "local_pty": "NATIVE_EVIDENCE_UNQUALIFIED",
                "other": "UNSUPPORTED_BACKEND"
            },
            "enrollment_available": false,
            "input_available": false
        })
    );
    assert!(schema_matches(
        &capability["outputSchema"],
        &json!({
            "status":"ok",
            "data":{
                "presentation":{"available":true, "reason":null},
                "backends":{
                    "local_pty":"AVAILABLE",
                    "other":"UNSUPPORTED_BACKEND"
                },
                "enrollment_available":true,
                "input_available":false
            }
        })
    ));
    let supported_result = json!({
        "status":"ok",
        "data":{"target_ref":"opaque exact target","presentation_revision":"a".repeat(48)}
    });
    assert!(schema_matches(&present["outputSchema"], &supported_result));
    assert!(schema_matches(
        &present["outputSchema"],
        &json!({"status":"refused", "code":"FOREGROUND_REFUSED"})
    ));
    let refused = tool_error(
        machine.process_value(call(
            json!(4),
            "festerm_workspace_present",
            json!({"enrollment_ref": "untrusted", "timeout_ms": 5000}),
        )),
        "UNAVAILABLE",
    );
    assert!(schema_matches(
        &present["outputSchema"],
        &refused["result"]["structuredContent"]
    ));
    assert_eq!(machine.in_flight(), 0);
    for arguments in [
        json!({}),
        json!({"enrollment_ref": ""}),
        json!({"enrollment_ref": "x", "timeout_ms": 0}),
        json!({"enrollment_ref": "x", "timeout_ms": 5001}),
        json!({"enrollment_ref": "x", "claimed_gesture": true}),
        json!({"enrollment_ref": "x".repeat(257)}),
    ] {
        tool_error(
            machine.process_value(call(json!(5), "festerm_workspace_present", arguments)),
            "INVALID_ARGUMENTS",
        );
    }
    assert_eq!(machine.in_flight(), 0);
}

fn assert_strict_object(schema: &Value, payload: &Value, pointer: &str) {
    let object = payload.pointer(pointer).unwrap().as_object().unwrap();
    for (key, member) in object {
        let mut missing = payload.clone();
        missing
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(key);
        assert!(
            !schema_matches(schema, &missing),
            "accepted missing {pointer}/{key}"
        );

        let wrong_type = match member {
            Value::Null | Value::Object(_) => json!([]),
            Value::Bool(_) | Value::Number(_) => json!("wrong type"),
            Value::String(_) => json!(42),
            Value::Array(_) => json!({}),
        };
        let mut invalid = payload.clone();
        invalid
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()[key] = wrong_type;
        assert!(
            !schema_matches(schema, &invalid),
            "accepted wrong type for {pointer}/{key}"
        );
    }
    let mut extra = payload.clone();
    extra
        .pointer_mut(pointer)
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("unexpected".into(), json!("must not be accepted"));
    assert!(
        !schema_matches(schema, &extra),
        "accepted extra field in {pointer}"
    );
}

#[test]
fn handshake_sequence_negotiation_and_ping() {
    let mut machine = Machine::new();
    error(
        machine.process_value(request(json!(2), "tools/list", json!({}))),
        -32002,
    );
    assert_eq!(machine.lifecycle(), Lifecycle::New);
    let ping = reply(machine.process_value(request(json!("ping"), "ping", json!({}))));
    assert_eq!(ping, json!({"jsonrpc":"2.0","id":"ping","result":{}}));
    assert_eq!(machine.lifecycle(), Lifecycle::New);
    let init = initialize(&mut machine, "unsupported-newer-version");
    assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(
        init["result"]["capabilities"],
        json!({"tools":{"listChanged":false}})
    );
    assert_eq!(machine.lifecycle(), Lifecycle::AwaitingInitialized);
    error(
        machine.process_value(request(json!(2), "tools/list", json!({}))),
        -32002,
    );
    // A request with the notification's name is not an initialized notification.
    error(
        machine.process_value(request(json!(2), "notifications/initialized", json!({}))),
        -32002,
    );
    machine.process_value(json!({
        "jsonrpc":"2.0","method":"notifications/initialized","params":{"approved":true}
    }));
    assert_eq!(machine.lifecycle(), Lifecycle::AwaitingInitialized);
    machine.process_value(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    assert_eq!(machine.lifecycle(), Lifecycle::Ready);
    let init_again = request(
        json!(3),
        "initialize",
        json!({
            "protocolVersion":PROTOCOL_VERSION,
            "capabilities":{}, "clientInfo":{"name":"x","version":"0"}
        }),
    );
    error(machine.process_value(init_again), -32600);
    assert_eq!(machine.in_flight(), 0);
}

#[test]
fn initialization_requires_params_and_discards_untrusted_metadata() {
    for params in [
        Value::Null,
        json!({}),
        json!({"protocolVersion":PROTOCOL_VERSION,"capabilities":{}}),
        json!({"protocolVersion":true,"capabilities":{},"clientInfo":{"name":"x","version":"0"}}),
        json!({"protocolVersion":PROTOCOL_VERSION,"capabilities":null,"clientInfo":{"name":"x","version":"0"}}),
        json!({"protocolVersion":PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":"x","version":"0","approved":true}}),
        json!({"protocolVersion":PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":"x","version":"0"},"grantId":"secret"}),
    ] {
        let mut machine = Machine::new();
        let response = error(
            machine.process_value(request(json!(1), "initialize", params)),
            -32602,
        );
        assert!(!response.to_string().contains("secret"));
        assert_eq!(machine.lifecycle(), Lifecycle::New);
    }
    let mut machine = Machine::new();
    let init = reply(machine.process_value(request(
        json!(1),
        "initialize",
        json!({
            "protocolVersion":PROTOCOL_VERSION,
            "capabilities":{
                "experimental":{"approved":true,"grantId":"must-not-leak"},
                "roots":{"listChanged":true},
                "sampling":{},
                "elicitation":{"form":{},"url":{}},
                "tasks":{"requests":{"sampling":{"createMessage":{}}}}
            },
            "clientInfo":{
                "name":"must-not-leak", "version":"0", "title":"must-not-leak",
                "description":"must-not-leak", "websiteUrl":"must-not-leak",
                "icons":[{"src":"must-not-leak","mimeType":"image/png","sizes":["48x48"],"theme":"dark"}]
            },
            "_meta":{"progressToken":"must-not-leak"}
        }),
    )));
    assert!(!init.to_string().contains("must-not-leak"));
    assert_eq!(machine.in_flight(), 0);
}

#[test]
fn wire_catalog_contains_only_exact_observation_schemas_and_hints() {
    let mut machine = ready();
    let catalog = reply(
        machine
            .process_bytes(b"{\"jsonrpc\":\"2.0\",\"id\":\"catalog\",\"method\":\"tools/list\"}\n"),
    );
    let tools = catalog["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 7);
    let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "festerm_pairing_status",
            "festerm_sessions_list",
            "festerm_sessions_get",
            "festerm_terminal_read_screen",
            "festerm_terminal_read_scrollback",
            "festerm_terminal_get_selection",
            "festerm_terminal_wait_for_change"
        ]
    );
    for tool in tools {
        assert_eq!(
            tool["annotations"],
            json!({
                "readOnlyHint":true,"destructiveHint":false,
                "openWorldHint":false,"idempotentHint":true
            })
        );
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert_eq!(tool["inputSchema"]["type"], "object");
    }
    assert_eq!(
        tools[4]["inputSchema"]["properties"],
        json!({
            "session_ref":{
                "type":"string","minLength":1,"maxLength":256,
                "description":"Opaque backend-validated reference, not authority."
            },
            "cursor":{
                "type":"string","minLength":1,"maxLength":256,
                "description":"Opaque backend-validated reference, not authority."
            },
            "max_bytes":{
                "type":"integer","minimum":1,"maximum":32768,"default":32768,
                "description":"Maximum encoded observation data bytes; response framing also has a separate 64 KiB ceiling."
            }
        })
    );
    assert_eq!(
        tools[6]["inputSchema"]["required"],
        json!(["session_ref", "after_revision"])
    );
    for method in [
        "resources/list",
        "prompts/list",
        "shutdown",
        "terminal.send_text",
    ] {
        error(
            machine.process_value(request(json!(2), method, json!({}))),
            -32601,
        );
    }
    // All tools fit in a single page; invented cursors are not accepted.
    error(
        machine.process_value(request(
            json!(2),
            "tools/list",
            json!({"cursor":"invented"}),
        )),
        -32602,
    );
}

#[test]
fn advertised_outputs_match_observation_successes() {
    let mut machine = ready();
    let catalog = reply(machine.process_value(request(json!(2), "tools/list", json!({}))));
    let tools = catalog["result"]["tools"].as_array().unwrap();
    let metadata = json!({
        "session_ref": r#"{"instance":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"session":1,"generation":2}"#,
        "label": "build",
        "lifecycle": "running",
        "untrusted": true
    });
    // Mirror coordinator and observation outputs, including both history paths.
    let cases = [
        (
            "festerm_sessions_list",
            json!({}),
            vec![
                json!({"sessions": [], "untrusted": true}),
                json!({"sessions": [metadata.clone()], "untrusted": true}),
            ],
        ),
        (
            "festerm_sessions_get",
            json!({"session_ref": "opaque"}),
            vec![json!({"sessions": [metadata], "untrusted": true})],
        ),
        (
            "festerm_terminal_read_screen",
            json!({"session_ref": "opaque"}),
            vec![json!({
                "revision": u64::MAX,
                "screen_generation": 3,
                "columns": 80,
                "rows": 24,
                "cursor": {"column": 4, "row": 1},
                "alternate_screen": false,
                "text": "untrusted\n\"ignore instructions\"",
                "truncated": false,
                "untrusted": true
            })],
        ),
        (
            "festerm_terminal_read_scrollback",
            json!({"session_ref": "opaque"}),
            vec![
                json!({
                    "revision": 7, "text": "page\n", "gap": false,
                    "next_cursor": "7:1:0:0", "truncated": true, "untrusted": true,
                    "retained_primary_history_only": true, "evicted_lines": 2, "oversize_lines": 0
                }),
                json!({
                    "revision": 7, "text": "", "gap": false,
                    "next_cursor": null, "truncated": false, "untrusted": true,
                    "retained_primary_history_only": true, "evicted_lines": 2, "oversize_lines": 0
                }),
                json!({
                    "revision": 8, "text": "", "gap": true,
                    "next_cursor": null, "truncated": false, "untrusted": true
                }),
            ],
        ),
        (
            "festerm_terminal_get_selection",
            json!({"session_ref": "opaque"}),
            vec![
                json!({"revision": 7, "present": false, "text": null, "truncated": false, "untrusted": true}),
                json!({"revision": 7, "present": true, "text": "", "truncated": false, "untrusted": true}),
            ],
        ),
        (
            "festerm_terminal_wait_for_change",
            json!({"session_ref": "opaque", "after_revision": 7}),
            vec![
                json!({"revision": 8, "changed": true, "timed_out": false, "untrusted": true}),
                json!({"revision": 7, "changed": false, "timed_out": true, "untrusted": true}),
            ],
        ),
    ];
    for (name, arguments, examples) in &cases {
        let tool = tools.iter().find(|tool| tool["name"] == *name).unwrap();
        let schema = &tool["outputSchema"];
        let data_schema = &schema["oneOf"][0]["properties"]["data"];
        assert_eq!(schema["type"], "object");
        assert_eq!(data_schema["type"], "object");
        assert_eq!(data_schema["additionalProperties"], false);
        let advertised: BTreeSet<_> = data_schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let actual: BTreeSet<_> = examples
            .iter()
            .flat_map(|example| example.as_object().unwrap().keys().map(String::as_str))
            .collect();
        assert_eq!(advertised, actual, "incorrect properties for {name}");
        if name.starts_with("festerm_sessions_") {
            let limit = if *name == "festerm_sessions_get" {
                1
            } else {
                32
            };
            assert_eq!(data_schema["properties"]["sessions"]["maxItems"], limit);
            let entry = examples
                .iter()
                .find_map(|example| example["sessions"].as_array().unwrap().first())
                .unwrap()
                .clone();
            let too_many = json!({
                "status": "ok",
                "data": {"sessions": vec![entry; limit as usize + 1], "untrusted": true}
            });
            assert!(!schema_matches(schema, &too_many));
        }

        for data in examples {
            let pending = backend(machine.process_value(call(json!(3), name, arguments.clone())));
            let response = decode(
                machine
                    .complete(
                        &pending.key,
                        BackendResult::AuthorizedObservation(data.clone()),
                    )
                    .unwrap(),
            );
            let structured = &response["result"]["structuredContent"];
            assert_eq!(response["result"]["isError"], false);
            assert_eq!(structured, &json!({"status": "ok", "data": data}));
            assert!(schema_matches(schema, structured), "{name}: {structured}");
            assert_eq!(
                serde_json::from_str::<Value>(
                    response["result"]["content"][0]["text"].as_str().unwrap()
                )
                .unwrap(),
                *structured
            );
            assert_strict_object(schema, structured, "");
            assert_strict_object(schema, structured, "/data");
            if name.starts_with("festerm_sessions_")
                && !data["sessions"].as_array().unwrap().is_empty()
            {
                assert_strict_object(schema, structured, "/data/sessions/0");
            }
            if *name == "festerm_terminal_read_screen" {
                assert_strict_object(schema, structured, "/data/cursor");
                let mut negative_revision = structured.clone();
                negative_revision["data"]["revision"] = json!(-1);
                assert!(!schema_matches(schema, &negative_revision));
            }
            let mut inconsistent = structured.clone();
            match *name {
                "festerm_terminal_read_scrollback" => {
                    if data["gap"] == true {
                        inconsistent["data"]["evicted_lines"] = json!(0);
                    } else {
                        inconsistent["data"]["truncated"] =
                            json!(!data["truncated"].as_bool().unwrap());
                    }
                }
                "festerm_terminal_get_selection" => {
                    inconsistent["data"]["present"] = json!(!data["present"].as_bool().unwrap());
                }
                "festerm_terminal_wait_for_change" => {
                    inconsistent["data"]["changed"] = json!(!data["changed"].as_bool().unwrap());
                }
                _ => {
                    inconsistent["data"]["untrusted"] = json!(false);
                }
            }
            assert!(
                !schema_matches(schema, &inconsistent),
                "{name}: {inconsistent}"
            );
        }
    }
}

#[test]
fn advertised_refusals_remain_content_free_for_every_tool() {
    let mut machine = ready();
    let catalog = reply(machine.process_value(request(json!(2), "tools/list", json!({}))));
    let tools = catalog["result"]["tools"].as_array().unwrap();
    let refusal_schema = json!({
        "properties": {
            "status": {"const": "refused"},
            "code": {"type": "string", "enum": [
                "NOT_AUTHORIZED", "STALE_TARGET", "CLOSED", "BUDGET_EXCEEDED",
                "INVALID_ARGUMENTS", "INVALID_BACKEND_RESULT", "RESPONSE_TOO_LARGE", "UNAVAILABLE"
            ]}
        },
        "required": ["status", "code"],
        "additionalProperties": false
    });
    for tool in tools {
        let schema = &tool["outputSchema"];
        assert_eq!(schema["oneOf"][1], refusal_schema);
        let name = tool["name"].as_str().unwrap();
        let arguments = match name {
            "festerm_pairing_status" | "festerm_sessions_list" => json!({}),
            "festerm_terminal_wait_for_change" => {
                json!({"session_ref": "opaque", "after_revision": 7})
            }
            _ => json!({"session_ref": "opaque"}),
        };

        for (reason, code) in [
            (Refusal::NotAuthorized, "NOT_AUTHORIZED"),
            (Refusal::StaleTarget, "STALE_TARGET"),
            (Refusal::Closed, "CLOSED"),
            (Refusal::BudgetExceeded, "BUDGET_EXCEEDED"),
            (Refusal::InvalidArguments, "INVALID_ARGUMENTS"),
            (Refusal::InvalidBackendResult, "INVALID_BACKEND_RESULT"),
            (Refusal::ResponseTooLarge, "RESPONSE_TOO_LARGE"),
            (Refusal::Unavailable, "UNAVAILABLE"),
        ] {
            let pending = backend(machine.process_value(call(json!(4), name, arguments.clone())));
            let response = decode(
                machine
                    .complete(&pending.key, BackendResult::Refused(reason))
                    .unwrap(),
            );
            let structured = &response["result"]["structuredContent"];
            assert_eq!(response["result"]["isError"], true);
            assert_eq!(structured, &json!({"status": "refused", "code": code}));
            assert!(schema_matches(schema, structured));
            let mut leaked = structured.clone();
            leaked["data"] = json!({"text": "must not be accepted"});
            assert!(!schema_matches(schema, &leaked));
        }
        assert!(!schema_matches(
            schema,
            &json!({"status": "refused", "code": "UNKNOWN"})
        ));
    }
}

#[test]
fn every_tool_maps_to_typed_operations_without_authority() {
    let mut machine = ready();
    let cases = [
        (
            "festerm_pairing_status",
            json!({}),
            Operation::PairingStatus,
        ),
        ("festerm_sessions_list", json!({}), Operation::SessionsList),
        (
            "festerm_sessions_get",
            json!({"session_ref":"opaque"}),
            Operation::SessionsGet {
                session_ref: "opaque".into(),
            },
        ),
        (
            "festerm_terminal_read_screen",
            json!({"session_ref":"opaque"}),
            Operation::TerminalReadScreen {
                session_ref: "opaque".into(),
                max_bytes: MAX_READ_BYTES,
            },
        ),
        (
            "festerm_terminal_read_scrollback",
            json!({"session_ref":"opaque","cursor":"page","max_bytes":123}),
            Operation::TerminalReadScrollback {
                session_ref: "opaque".into(),
                cursor: Some("page".into()),
                max_bytes: 123,
            },
        ),
        (
            "festerm_terminal_get_selection",
            json!({"session_ref":"opaque","max_bytes":8}),
            Operation::TerminalGetSelection {
                session_ref: "opaque".into(),
                max_bytes: 8,
            },
        ),
        (
            "festerm_terminal_wait_for_change",
            json!({"session_ref":"opaque","after_revision":u64::MAX}),
            Operation::TerminalWaitForChange {
                session_ref: "opaque".into(),
                after_revision: u64::MAX,
                timeout_ms: MAX_WAIT_MS,
            },
        ),
    ];
    for (name, args, expected) in cases {
        let request = backend(machine.process_value(call(json!(2), name, args)));
        assert_eq!(request.operation, expected);
        let result = if expected == Operation::PairingStatus {
            BackendResult::PairingStatus(PairingStatus {
                paired: false,
                observation_enabled: false,
                verification_code: VerificationCode::new("0123456789abcdef").unwrap(),
            })
        } else {
            BackendResult::Refused(Refusal::NotAuthorized)
        };
        assert!(machine.complete(&request.key, result).is_some());
        assert_eq!(machine.in_flight(), 0);
    }
}

#[test]
fn operation_schemas_refuse_unknown_fields_invalid_types_and_limits() {
    let mut machine = ready();
    for (tool, args) in [
        ("festerm_pairing_status", json!({"approved":true})),
        ("festerm_sessions_list", json!({"grantId":"secret"})),
        ("festerm_sessions_get", json!({})),
        ("festerm_sessions_get", json!({"session_ref":""})),
        ("festerm_sessions_get", json!({"session_ref":42})),
        (
            "festerm_sessions_get",
            json!({"session_ref":"x".repeat(257)}),
        ),
        (
            "festerm_terminal_read_screen",
            json!({"session_ref":"x","max_bytes":0}),
        ),
        (
            "festerm_terminal_read_screen",
            json!({"session_ref":"x","max_bytes":32769}),
        ),
        (
            "festerm_terminal_read_screen",
            json!({"session_ref":"x","max_bytes":1.5}),
        ),
        (
            "festerm_terminal_read_screen",
            json!({"session_ref":"x","max_bytes":null}),
        ),
        (
            "festerm_terminal_read_screen",
            json!({"session_ref":"x","approved":true}),
        ),
        (
            "festerm_terminal_read_scrollback",
            json!({"session_ref":"x","cursor":null}),
        ),
        (
            "festerm_terminal_read_scrollback",
            json!({"session_ref":"x","cursor":""}),
        ),
        (
            "festerm_terminal_wait_for_change",
            json!({"session_ref":"x","after_revision":-1}),
        ),
        (
            "festerm_terminal_wait_for_change",
            json!({"session_ref":"x","after_revision":1.0}),
        ),
        (
            "festerm_terminal_wait_for_change",
            json!({"session_ref":"x","after_revision":0,"timeout_ms":30001}),
        ),
        (
            "festerm_terminal_wait_for_change",
            json!({"session_ref":"x","after_revision":0,"timeout_ms":0}),
        ),
    ] {
        let value = tool_error(
            machine.process_value(call(json!(2), tool, args)),
            "INVALID_ARGUMENTS",
        );
        assert!(!value.to_string().contains("secret"));
        assert_eq!(machine.in_flight(), 0);
    }
    for params in [
        Value::Null,
        json!({}),
        json!({"name":null}),
        json!({"name":"festerm_sessions_list","arguments":null}),
        json!({"name":"festerm_sessions_list","arguments":[]}),
        json!({"name":"festerm_sessions_list","grantId":"secret"}),
        json!({"name":"festerm_terminal_send_text","arguments":{"text":"secret"}}),
        json!({"name":"festerm_sessions_list","task":{"ttl":100}}),
    ] {
        let response = error(
            machine.process_value(request(json!(2), "tools/call", params)),
            -32602,
        );
        assert!(!response.to_string().contains("secret"));
    }
    let request = backend(machine.process_value(request(
        json!(2),
        "tools/call",
        json!({"name":"festerm_sessions_list","_meta":{"progressToken":1}}),
    )));
    assert_eq!(request.operation, Operation::SessionsList);
}

#[test]
fn notifications_never_dispatch_tools_or_emit_replies() {
    let mut machine = Machine::new();
    for method in [
        "initialize",
        "notifications/initialized",
        "tools/call",
        "ping",
        "unknown",
    ] {
        assert!(matches!(
            machine.process_value(json!({
                "jsonrpc":"2.0","method":method,
                "params":{"name":"festerm_sessions_list","arguments":{}}
            })),
            Event::NoReply
        ));
    }
    assert_eq!(machine.lifecycle(), Lifecycle::New);
    let mut machine = ready();
    for notification in [
        json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"festerm_sessions_list"}}),
        json!({"jsonrpc":"2.0","method":"tools/call","params":null}),
        json!({"jsonrpc":"2.0","method":"unknown"}),
        json!({"jsonrpc":"2.0","method":"ping","extra":"unknown"}),
    ] {
        assert!(matches!(
            machine.process_value(notification),
            Event::NoReply
        ));
    }
    assert_eq!(machine.in_flight(), 0);
}

#[test]
fn cancellation_frees_capacity_ignores_reason_and_rejects_late_generations() {
    let mut machine = ready();
    let first = backend(machine.process_value(call(
        json!("id"),
        "festerm_terminal_wait_for_change",
        json!({"session_ref":"opaque","after_revision":0,"timeout_ms":1}),
    )));
    let cancel = machine.process_bytes(
        br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"id","reason":"secret terminal output"}}"#,
    );
    let Event::Cancelled(cancelled) = cancel else {
        panic!("expected cancellation")
    };
    assert_eq!(cancelled, first.key);
    assert_eq!(machine.in_flight(), 0);
    let second =
        backend(machine.process_value(call(json!("id"), "festerm_sessions_list", json!({}))));
    assert_ne!(first.key, second.key);
    assert!(machine
        .complete(
            &first.key,
            BackendResult::AuthorizedObservation(json!({"secret":"must not release"})),
        )
        .is_none());
    assert_eq!(machine.in_flight(), 1);
    for params in [
        Value::Null,
        json!({}),
        json!({"requestId":1.5}),
        json!({"requestId":"id","reason":true}),
        json!({"requestId":"id","approved":true}),
        json!({"requestId":"unknown"}),
    ] {
        assert!(matches!(
            machine.process_value(json!({
                "jsonrpc":"2.0","method":"notifications/cancelled","params":params
            })),
            Event::NoReply
        ));
        assert_eq!(machine.in_flight(), 1);
    }
    // A request cannot masquerade as a cancellation notification.
    error(
        machine.process_value(request(
            json!(5),
            "notifications/cancelled",
            json!({"requestId":"id"}),
        )),
        -32601,
    );
    assert_eq!(machine.in_flight(), 1);
    let response = machine
        .complete(&second.key, BackendResult::Refused(Refusal::NotAuthorized))
        .unwrap();
    assert_eq!(decode(response)["result"]["isError"], true);
    assert!(machine
        .complete(&second.key, BackendResult::Refused(Refusal::NotAuthorized))
        .is_none());
}

#[test]
fn duplicate_ids_and_in_flight_budget_are_bounded() {
    let mut machine = ready();
    let mut pending = Vec::new();
    for id in 0..MAX_IN_FLIGHT {
        pending.push(backend(machine.process_value(call(
            json!(id),
            "festerm_sessions_list",
            json!({}),
        ))));
    }
    assert_eq!(machine.in_flight(), MAX_IN_FLIGHT);
    error(
        machine.process_value(call(json!(0), "festerm_pairing_status", json!({}))),
        -32003,
    );
    error(
        machine.process_value(request(json!(0), "ping", json!({}))),
        -32003,
    );
    error(
        machine.process_value(call(json!(5), "festerm_sessions_list", json!({}))),
        -32004,
    );
    assert_eq!(machine.in_flight(), MAX_IN_FLIGHT);
    machine.complete(
        &pending.remove(0).key,
        BackendResult::Refused(Refusal::NotAuthorized),
    );
    pending.push(backend(machine.process_value(call(
        json!("0"),
        "festerm_sessions_list",
        json!({}),
    ))));
    assert_eq!(machine.in_flight(), MAX_IN_FLIGHT);
    let cancelled = machine.close();
    assert_eq!(cancelled.len(), MAX_IN_FLIGHT);
    assert_eq!(machine.in_flight(), 0);
    assert_eq!(machine.lifecycle(), Lifecycle::Closed);
    assert!(machine.close().is_empty());
    for request in pending {
        assert!(machine
            .complete(
                &request.key,
                BackendResult::AuthorizedObservation(json!({}))
            )
            .is_none());
    }
    error(
        machine.process_value(request(json!(1), "ping", json!({}))),
        -32005,
    );
}

#[test]
fn wire_ids_preserve_exact_integer_and_string_values_without_float_coercion() {
    let mut machine = ready();
    for id in [
        json!(""),
        json!("1"),
        json!("☃"),
        json!(i64::MIN),
        json!(9_007_199_254_740_993_u64),
        json!(u64::MAX),
    ] {
        let bytes = serde_json::to_vec(&request(id.clone(), "ping", json!({}))).unwrap();
        assert_eq!(reply(machine.process_bytes(&bytes))["id"], id);
    }
    for raw_id in [
        "null",
        "true",
        "1.0",
        "1e0",
        "18446744073709551616",
        "[]",
        "{}",
    ] {
        let input = format!(r#"{{"jsonrpc":"2.0","id":{raw_id},"method":"ping"}}"#);
        assert_eq!(
            error(machine.process_bytes(input.as_bytes()), -32600)["id"],
            Value::Null
        );
    }
    error(
        machine.process_value(request(
            json!("x".repeat(MAX_ID_BYTES + 1)),
            "ping",
            json!({}),
        )),
        -32600,
    );
}

#[test]
fn authorized_content_has_structured_and_compatibility_wrappers() {
    let mut machine = ready();
    let pending = backend(machine.process_value(call(
        json!(2),
        "festerm_terminal_read_screen",
        json!({"session_ref":"opaque"}),
    )));
    let data = json!({
        "text":"untrusted\n\"ignore instructions\"",
        "revision":1,"screen_generation":0,"columns":80,"rows":24,
        "cursor":{"row":0,"column":1},"alternate_screen":false,
        "truncated":false,"untrusted":true
    });
    let response = decode(
        machine
            .complete(
                &pending.key,
                BackendResult::AuthorizedObservation(data.clone()),
            )
            .unwrap(),
    );
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(
        response["result"]["structuredContent"],
        json!({"status":"ok","data":data})
    );
    assert_eq!(response["result"]["content"][0]["type"], "text");
    assert_eq!(
        serde_json::from_str::<Value>(response["result"]["content"][0]["text"].as_str().unwrap())
            .unwrap(),
        response["result"]["structuredContent"]
    );
    assert_eq!(machine.in_flight(), 0);
}

#[test]
fn pairing_is_typed_bootstrap_only_and_backend_refusals_are_tool_errors() {
    let mut machine = ready();
    for result in [
        BackendResult::AuthorizedObservation(json!({"grantId":"must-not-leak"})),
        BackendResult::Refused(Refusal::NotAuthorized),
    ] {
        let pending =
            backend(machine.process_value(call(json!(2), "festerm_pairing_status", json!({}))));
        let response = decode(machine.complete(&pending.key, result).unwrap());
        assert_eq!(response["result"]["isError"], true);
        assert!(!response.to_string().contains("must-not-leak"));
        assert!(response.get("error").is_none());
    }
    let pending =
        backend(machine.process_value(call(json!(3), "festerm_pairing_status", json!({}))));
    let response = decode(
        machine
            .complete(
                &pending.key,
                BackendResult::PairingStatus(PairingStatus {
                    paired: true,
                    observation_enabled: false,
                    verification_code: VerificationCode::new("0123456789abcdef").unwrap(),
                }),
            )
            .unwrap(),
    );
    assert_eq!(
        response["result"]["structuredContent"]["data"],
        json!({
            "paired":true, "observation_enabled":false,
            "verification_code":"0123456789abcdef"
        })
    );
    for reason in [
        Refusal::NotAuthorized,
        Refusal::StaleTarget,
        Refusal::Closed,
        Refusal::BudgetExceeded,
        Refusal::Unavailable,
    ] {
        let pending =
            backend(machine.process_value(call(json!(4), "festerm_sessions_list", json!({}))));
        let response = decode(
            machine
                .complete(&pending.key, BackendResult::Refused(reason))
                .unwrap(),
        );
        assert_eq!(response["result"]["isError"], true);
        assert!(response["result"]["structuredContent"]
            .get("data")
            .is_none());
    }
}

#[test]
fn verification_code_accepts_only_canonical_fixed_lowercase_hex() {
    let code = VerificationCode::new("0123456789abcdef").unwrap();
    assert_eq!(code.as_str(), "0123456789abcdef");
    for invalid in [
        "",
        "0123456789abcde",
        "0123456789abcdef0",
        "0123456789ABCDEF",
        "0123456789abcdeg",
        " 123456789abcdef",
        "0123456789abcde\n",
        "0123456789abcde\0",
        "0x23456789abcdef",
        "é123456789abcde",
        "arbitrary backend exception text",
    ] {
        assert!(VerificationCode::new(invalid).is_none());
    }
}

#[test]
fn pairing_publishes_required_public_code_schema_and_matching_result() {
    let mut machine = ready();
    let catalog = reply(machine.process_value(request(json!(2), "tools/list", json!({}))));
    let pairing = &catalog["result"]["tools"][0];
    let success_schema = &pairing["outputSchema"]["oneOf"][0];
    assert_eq!(
        success_schema["properties"]["data"]["required"],
        json!(["paired", "observation_enabled", "verification_code"])
    );
    assert_eq!(
        success_schema["properties"]["data"]["properties"]["verification_code"],
        json!({
            "type":"string", "minLength":16, "maxLength":16,
            "pattern":"^[0-9a-f]{16}$",
            "description":"Parent-generated public fingerprint for this exact private channel. Not a secret, principal, credential or grant; useless on another stream."
        })
    );
    assert_eq!(
        success_schema["properties"]["data"]["additionalProperties"],
        false
    );
    let pending =
        backend(machine.process_value(call(json!(3), "festerm_pairing_status", json!({}))));
    let response = decode(
        machine
            .complete(
                &pending.key,
                BackendResult::PairingStatus(PairingStatus {
                    paired: false,
                    observation_enabled: false,
                    verification_code: VerificationCode::new("fedcba9876543210").unwrap(),
                }),
            )
            .unwrap(),
    );
    let structured = &response["result"]["structuredContent"];
    assert_eq!(
        structured,
        &json!({
            "status":"ok",
            "data":{
                "paired":false, "observation_enabled":false,
                "verification_code":"fedcba9876543210"
            }
        })
    );
    assert_eq!(
        serde_json::from_str::<Value>(response["result"]["content"][0]["text"].as_str().unwrap())
            .unwrap(),
        *structured
    );
    assert_eq!(machine.in_flight(), 0);
}

#[test]
fn copying_public_verification_code_never_grants_or_requests_approval() {
    let mut original = ready();
    let pending =
        backend(original.process_value(call(json!(2), "festerm_pairing_status", json!({}))));
    original
        .complete(
            &pending.key,
            BackendResult::PairingStatus(PairingStatus {
                paired: true,
                observation_enabled: true,
                verification_code: VerificationCode::new("0123456789abcdef").unwrap(),
            }),
        )
        .unwrap();
    let mut other = ready();
    for machine in [&mut original, &mut other] {
        tool_error(
            machine.process_value(call(
                json!(3),
                "festerm_pairing_status",
                json!({"verification_code":"0123456789abcdef"}),
            )),
            "INVALID_ARGUMENTS",
        );
        let pending = backend(machine.process_value(request(
            json!(4),
            "tools/call",
            json!({
                "name":"festerm_sessions_list",
                "_meta":{"verification_code":"0123456789abcdef","approved":true}
            }),
        )));
        assert_eq!(pending.operation, Operation::SessionsList);
        let response = decode(
            machine
                .complete(&pending.key, BackendResult::Refused(Refusal::NotAuthorized))
                .unwrap(),
        );
        assert_eq!(
            response["result"]["structuredContent"],
            json!({"status":"refused","code":"NOT_AUTHORIZED"})
        );
        assert!(!response.to_string().contains("0123456789abcdef"));
    }
}

#[test]
fn mismatched_and_non_object_backend_results_are_content_free_refusals() {
    let mut machine = ready();
    for result in [
        BackendResult::PairingStatus(PairingStatus {
            paired: true,
            observation_enabled: true,
            verification_code: VerificationCode::new("0123456789abcdef").unwrap(),
        }),
        BackendResult::AuthorizedObservation(json!("private exception message")),
        BackendResult::AuthorizedObservation(json!(null)),
    ] {
        let pending =
            backend(machine.process_value(call(json!(2), "festerm_sessions_list", json!({}))));
        let response = decode(machine.complete(&pending.key, result).unwrap());
        assert_eq!(
            response["result"]["structuredContent"],
            json!({"status":"refused","code":"INVALID_BACKEND_RESULT"})
        );
        assert_eq!(response["result"]["isError"], true);
        assert!(!response.to_string().contains("private"));
        assert_eq!(machine.in_flight(), 0);
    }
}

#[test]
fn malformed_batches_unknown_fields_and_duplicate_members_never_dispatch() {
    let mut machine = ready();
    for bytes in [
        b"{".as_slice(),
        b"\xff".as_slice(),
        b"{}{}".as_slice(),
        br#"{"jsonrpc":"2.0","id":1,"method":"ping","method":"tools/call"}"#.as_slice(),
        br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"festerm_sessions_get","arguments":{"session_ref":"a","session_ref":"b"}}}"#.as_slice(),
        b"{\n\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}".as_slice(),
    ] {
        let response = error(machine.process_bytes(bytes), -32700);
        assert_eq!(response["id"], Value::Null);
    }
    for value in [
        json!([]),
        json!([{"jsonrpc":"2.0","id":1,"method":"ping"}]),
        json!(null),
        json!({}),
        json!({"jsonrpc":"1.0","id":1,"method":"ping"}),
        json!({"jsonrpc":"2.0","id":1,"result":{}}),
        json!({"jsonrpc":"2.0","id":1,"method":false}),
        json!({"jsonrpc":"2.0","id":1,"method":"ping","approved":true}),
    ] {
        error(machine.process_value(value), -32600);
    }
    for params in [json!({"extra":"secret"}), json!(null), json!([])] {
        error(
            machine.process_value(request(json!(1), "ping", params)),
            -32602,
        );
    }
    assert_eq!(machine.in_flight(), 0);
}

fn nested(depth: usize) -> Value {
    (0..depth).fold(json!(true), |value, _| json!([value]))
}

#[test]
fn depth_collection_string_node_and_encoded_input_bounds_apply_to_both_apis() {
    let mut machine = ready();
    let values = [
        nested(MAX_DEPTH + 1),
        json!(vec![true; MAX_COLLECTION_ITEMS + 1]),
        json!("x".repeat(MAX_STRING_BYTES + 1)),
        json!(vec![
            vec![true; MAX_COLLECTION_ITEMS];
            MAX_VALUE_NODES / MAX_COLLECTION_ITEMS + 1
        ]),
    ];
    for value in values {
        let bytes = serde_json::to_vec(&value).unwrap();
        error(machine.process_bytes(&bytes), -32700);
        error(machine.process_value(value), -32006);
    }
    error(
        machine.process_bytes(&vec![b' '; MAX_MESSAGE_BYTES + 1]),
        -32006,
    );
    // Every individual string fits, but the entire request exceeds 64 KiB.
    let value = request(
        json!(1),
        "ping",
        json!({"_meta":{"a":"x".repeat(MAX_STRING_BYTES),"b":"x".repeat(MAX_STRING_BYTES)}}),
    );
    error(machine.process_value(value), -32006);
    // A deeply nested attack is stopped at the limit, not serde's recursion cap.
    let deep = format!("{}true{}", "[".repeat(10_000), "]".repeat(10_000));
    error(machine.process_bytes(deep.as_bytes()), -32700);
    assert_eq!(machine.in_flight(), 0);
}

#[test]
fn bounded_completion_refuses_whole_result_never_fragments_and_cleans_up() {
    let mut machine = ready();
    let pending = backend(machine.process_value(call(
        json!(2),
        "festerm_terminal_read_screen",
        json!({"session_ref":"opaque"}),
    )));
    let near_ceiling = decode(
        machine
            .complete(
                &pending.key,
                BackendResult::AuthorizedObservation(json!({"text":"x".repeat(32_000)})),
            )
            .unwrap(),
    );
    assert_eq!(near_ceiling["result"]["isError"], false);
    assert_eq!(
        near_ceiling["result"]["structuredContent"]["data"]["text"]
            .as_str()
            .unwrap()
            .len(),
        32_000
    );
    for data in [
        json!({"text":"SECRET".repeat(10_000)}),
        json!({"text":"\0".repeat(8_000)}),
        json!({"text":"\\".repeat(16_000)}),
        nested(MAX_DEPTH + 1),
        json!(vec![true; MAX_COLLECTION_ITEMS + 1]),
    ] {
        let pending = backend(machine.process_value(call(
            json!(2),
            "festerm_terminal_read_screen",
            json!({"session_ref":"opaque"}),
        )));
        let response = decode(
            machine
                .complete(&pending.key, BackendResult::AuthorizedObservation(data))
                .unwrap(),
        );
        assert_eq!(response["result"]["isError"], true);
        assert!(response["result"]["structuredContent"]
            .get("data")
            .is_none());
        assert!(!response.to_string().contains("SECRET"));
        assert_eq!(machine.in_flight(), 0);
    }
    // Encoded backend data fits in 32 KiB, but duplication + escaping does not.
    let pending = backend(machine.process_value(call(
        json!(2),
        "festerm_terminal_read_screen",
        json!({"session_ref":"opaque"}),
    )));
    let response = decode(
        machine
            .complete(
                &pending.key,
                BackendResult::AuthorizedObservation(json!({"text":"\0".repeat(5_100)})),
            )
            .unwrap(),
    );
    assert_eq!(
        response["result"]["structuredContent"]["code"],
        "RESPONSE_TOO_LARGE"
    );
    let pending = backend(machine.process_value(call(
        json!(2),
        "festerm_terminal_read_screen",
        json!({"session_ref":"opaque","max_bytes":10}),
    )));
    let response = decode(
        machine
            .complete(
                &pending.key,
                BackendResult::AuthorizedObservation(json!({"text":"more than ten bytes"})),
            )
            .unwrap(),
    );
    assert_eq!(
        response["result"]["structuredContent"]["code"],
        "RESPONSE_TOO_LARGE"
    );
}

#[test]
fn completions_cannot_cross_machine_boundaries_even_with_the_same_wire_id() {
    let mut first = ready();
    let mut second = ready();
    let first_request =
        backend(first.process_value(call(json!(2), "festerm_sessions_list", json!({}))));
    let second_request =
        backend(second.process_value(call(json!(2), "festerm_sessions_list", json!({}))));
    assert_ne!(first_request.key, second_request.key);
    assert!(second
        .complete(
            &first_request.key,
            BackendResult::AuthorizedObservation(json!({"private":"must not release"})),
        )
        .is_none());
    assert_eq!(second.in_flight(), 1);
}

#[test]
fn line_reader_stops_after_max_plus_one_and_handles_eof_crlf() {
    let mut giant = Cursor::new(vec![b'x'; MAX_MESSAGE_BYTES * 10]);
    assert_eq!(read_bounded_line(&mut giant), Err(LineError::TooLong));
    assert_eq!(giant.position(), (MAX_MESSAGE_BYTES + 1) as u64);

    let mut exact = vec![b' '; MAX_MESSAGE_BYTES];
    exact.push(b'\n');
    let mut exact = Cursor::new(exact);
    assert_eq!(
        read_bounded_line(&mut exact).unwrap().unwrap().len(),
        MAX_MESSAGE_BYTES
    );
    assert_eq!(read_bounded_line(&mut exact), Ok(None));

    let mut lines = Cursor::new(b"{}\r\n{}\n");
    assert_eq!(read_bounded_line(&mut lines), Ok(Some(b"{}".to_vec())));
    assert_eq!(read_bounded_line(&mut lines), Ok(Some(b"{}".to_vec())));
    assert_eq!(read_bounded_line(&mut lines), Ok(None));
    assert_eq!(
        read_bounded_line(&mut Cursor::new(b"{}")),
        Err(LineError::Unterminated)
    );
}

fn ready_controller() -> Machine {
    let mut machine = Machine::with_controller_enrollment();
    initialize(&mut machine, PROTOCOL_VERSION);
    assert!(matches!(
        machine.process_value(json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
        Event::NoReply
    ));
    machine
}

#[test]
fn controller_and_presentation_opt_ins_remain_separate_and_fail_closed() {
    let mut controller = ready_controller();
    let mut presentation = Machine::with_unavailable_presentation();
    initialize(&mut presentation, PROTOCOL_VERSION);
    assert!(matches!(
        presentation.process_value(json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
        Event::NoReply
    ));

    let controller_catalog =
        reply(controller.process_value(request(json!(2), "tools/list", json!({}))));
    let presentation_catalog =
        reply(presentation.process_value(request(json!(2), "tools/list", json!({}))));
    let controller_tools = controller_catalog["result"]["tools"].as_array().unwrap();
    let presentation_tools = presentation_catalog["result"]["tools"].as_array().unwrap();
    assert_eq!(&controller_tools[..7], &presentation_tools[..7]);
    assert!(controller_tools
        .iter()
        .any(|tool| tool["name"] == "festerm_controller_register"));
    assert!(!controller_tools
        .iter()
        .any(|tool| tool["name"] == "festerm_presentation_capabilities"));
    assert!(presentation_tools
        .iter()
        .any(|tool| tool["name"] == "festerm_presentation_capabilities"));
    assert!(!presentation_tools
        .iter()
        .any(|tool| tool["name"] == "festerm_controller_register"));

    error(
        controller.process_value(call(
            json!(3),
            "festerm_workspace_present",
            json!({"enrollment_ref": "opaque"}),
        )),
        -32602,
    );
    error(
        presentation.process_value(call(
            json!(3),
            "festerm_controller_register",
            json!({"local_id": "untrusted"}),
        )),
        -32602,
    );
    assert_eq!(controller.in_flight(), 0);
    assert_eq!(presentation.in_flight(), 0);
}

#[test]
fn combined_controller_catalog_reflects_live_capabilities_and_refuses_presentation() {
    let mut machine = Machine::with_controller_enrollment_and_unavailable_presentation();
    initialize(&mut machine, PROTOCOL_VERSION);
    assert!(matches!(
        machine.process_value(json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
        Event::NoReply
    ));
    let catalog = reply(machine.process_value(request(json!(2), "tools/list", json!({}))));
    let tools = catalog["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 13);
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "festerm_controller_register"));
    let capability = tools
        .iter()
        .find(|tool| tool["name"] == "festerm_presentation_capabilities")
        .unwrap();
    let schema = &capability["outputSchema"];
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "festerm_workspace_present"));

    for (id, enrollment_available) in [(3, true), (4, false)] {
        let pending = backend(machine.process_value(call(
            json!(id),
            "festerm_presentation_capabilities",
            json!({}),
        )));
        assert_eq!(pending.operation, Operation::PresentationCapabilities);
        let response = decode(
            machine
                .complete(
                    &pending.key,
                    BackendResult::ControllerCapabilities {
                        enrollment_available,
                        metadata_available: false,
                    },
                )
                .unwrap(),
        );
        let structured = &response["result"]["structuredContent"];
        assert_eq!(
            structured["data"],
            json!({
                "presentation": {"available": false, "reason": "NATIVE_EVIDENCE_UNQUALIFIED"},
                "backends": {
                    "local_pty": "NATIVE_EVIDENCE_UNQUALIFIED",
                    "other": "UNSUPPORTED_BACKEND"
                },
                "enrollment_available": enrollment_available,
                "input_available": false
            })
        );
        assert!(schema_matches(schema, structured));
    }

    let pending = backend(machine.process_value(call(
        json!(5),
        "festerm_presentation_capabilities",
        json!({}),
    )));
    tool_error(
        machine.process_value(call(
            json!(6),
            "festerm_workspace_present",
            json!({"enrollment_ref": "untrusted"}),
        )),
        "UNAVAILABLE",
    );
    assert_eq!(machine.in_flight(), 1);
    let Event::Cancelled(cancelled) = machine.process_value(json!({
        "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 5}
    })) else {
        panic!("expected cancellation");
    };
    assert_eq!(cancelled, pending.key);
    assert!(machine
        .complete(
            &pending.key,
            BackendResult::ControllerCapabilities {
                enrollment_available: true,
                metadata_available: false,
            },
        )
        .is_none());

    let pending = backend(machine.process_value(call(
        json!(7),
        "festerm_presentation_capabilities",
        json!({}),
    )));
    let response = decode(
        machine
            .complete(
                &pending.key,
                BackendResult::AuthorizedObservation(json!({"private": "must not leak"})),
            )
            .unwrap(),
    );
    let structured = &response["result"]["structuredContent"];
    assert_eq!(structured["code"], "INVALID_BACKEND_RESULT");
    assert!(!response.to_string().contains("must not leak"));
    assert!(schema_matches(schema, structured));

    for refusal in [
        Refusal::NotAuthorized,
        Refusal::StaleTarget,
        Refusal::Closed,
        Refusal::BudgetExceeded,
        Refusal::InvalidArguments,
        Refusal::InvalidBackendResult,
        Refusal::ResponseTooLarge,
        Refusal::Unavailable,
    ] {
        let pending = backend(machine.process_value(call(
            json!(8),
            "festerm_presentation_capabilities",
            json!({}),
        )));
        let response = decode(
            machine
                .complete(&pending.key, BackendResult::Refused(refusal))
                .unwrap(),
        );
        assert!(schema_matches(
            schema,
            &response["result"]["structuredContent"]
        ));
    }

    for id in 10..10 + MAX_IN_FLIGHT {
        backend(machine.process_value(call(
            json!(id),
            "festerm_presentation_capabilities",
            json!({}),
        )));
    }
    error(
        machine.process_value(call(
            json!(20),
            "festerm_presentation_capabilities",
            json!({}),
        )),
        -32004,
    );
    assert_eq!(machine.close().len(), MAX_IN_FLIGHT);
}

#[test]
fn controller_catalog_is_opt_in_and_preserves_observation_compatibility() {
    let mut original = ready();
    let mut controller = ready_controller();
    let original_catalog =
        reply(original.process_value(request(json!(2), "tools/list", json!({}))));
    let controller_catalog =
        reply(controller.process_value(request(json!(2), "tools/list", json!({}))));
    let original_tools = original_catalog["result"]["tools"].as_array().unwrap();
    let tools = controller_catalog["result"]["tools"].as_array().unwrap();
    assert_eq!(original_tools.len(), 7);
    assert_eq!(tools.len(), 11);
    assert_eq!(original_tools.as_slice(), &tools[..7]);
    let mut default = Machine::default();
    initialize(&mut default, PROTOCOL_VERSION);
    assert!(matches!(
        default.process_value(json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
        Event::NoReply
    ));
    let default_catalog = reply(default.process_value(request(json!(2), "tools/list", json!({}))));
    assert_eq!(
        original_catalog["result"]["tools"],
        default_catalog["result"]["tools"]
    );
    let names: Vec<_> = tools[7..]
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "festerm_controller_capabilities",
            "festerm_controller_register",
            "festerm_controller_status",
            "festerm_controller_unregister"
        ]
    );
    assert!(tools[9]["description"]
        .as_str()
        .unwrap()
        .contains("Metadata requires separate native authorization"));
    assert_eq!(
        tools[8]["annotations"],
        json!({
            "readOnlyHint":false,"destructiveHint":false,
            "openWorldHint":false,"idempotentHint":true
        })
    );
    assert_eq!(
        tools[10]["annotations"],
        json!({
            "readOnlyHint":false,"destructiveHint":true,
            "openWorldHint":false,"idempotentHint":false
        })
    );
    let registration_id = "12".repeat(16);
    for (name, arguments) in [
        ("festerm_controller_capabilities", json!({})),
        ("festerm_controller_register", json!({"local_id":"client"})),
        (
            "festerm_controller_status",
            json!({"registration_id": registration_id.as_str()}),
        ),
        (
            "festerm_controller_unregister",
            json!({"registration_id": registration_id.as_str()}),
        ),
    ] {
        error(
            original.process_value(call(json!(3), name, arguments)),
            -32602,
        );
        assert_eq!(original.in_flight(), 0);
    }
    assert!(matches!(
        controller.process_value(json!({
            "jsonrpc":"2.0", "method":"tools/call",
            "params":{"name":"festerm_controller_register","arguments":{"local_id":"client"}}
        })),
        Event::NoReply
    ));
    assert_eq!(controller.in_flight(), 0);

    let original_request =
        backend(original.process_value(call(json!(4), "festerm_sessions_list", json!({}))));
    let controller_request =
        backend(controller.process_value(call(json!(4), "festerm_sessions_list", json!({}))));
    let observation =
        || BackendResult::AuthorizedObservation(json!({"sessions":[],"untrusted":true}));
    assert_eq!(
        decode(
            original
                .complete(&original_request.key, observation())
                .unwrap()
        ),
        decode(
            controller
                .complete(&controller_request.key, observation())
                .unwrap()
        )
    );
}

#[test]
fn controller_inputs_are_bounded_and_do_not_accept_authority_claims() {
    let mut machine = ready_controller();
    for arguments in [
        json!({"local_id":"client", "gesture":true}),
        json!({"local_id":"client", "target":"tab 1"}),
        json!({"local_id":"client", "pid":1}),
        json!({"local_id":"client", "approve":true}),
    ] {
        tool_error(
            machine.process_value(call(json!(3), "festerm_controller_register", arguments)),
            "INVALID_ARGUMENTS",
        );
    }
    let registration_id = "12".repeat(16);
    for (tool, arguments) in [
        ("festerm_controller_capabilities", json!({"gesture":true})),
        (
            "festerm_controller_status",
            json!({"registration_id":registration_id.as_str(), "target":"tab 1"}),
        ),
        (
            "festerm_controller_unregister",
            json!({"registration_id":registration_id.as_str(), "approved":true}),
        ),
    ] {
        tool_error(
            machine.process_value(call(json!(3), tool, arguments)),
            "INVALID_ARGUMENTS",
        );
    }
    for local_id in [
        String::new(),
        "x".repeat(MAX_CONTROLLER_LOCAL_ID_BYTES + 1),
        "é".repeat(MAX_CONTROLLER_LOCAL_ID_BYTES / 2 + 1),
    ] {
        tool_error(
            machine.process_value(call(
                json!(4),
                "festerm_controller_register",
                json!({"local_id": local_id}),
            )),
            "INVALID_ARGUMENTS",
        );
    }
    for local_id in [
        "x".repeat(MAX_CONTROLLER_LOCAL_ID_BYTES),
        "é".repeat(MAX_CONTROLLER_LOCAL_ID_BYTES / 2),
        "private\n\u{202e}registration".to_owned(),
    ] {
        let pending = backend(machine.process_value(call(
            json!(5),
            "festerm_controller_register",
            json!({"local_id": local_id.as_str()}),
        )));
        assert!(matches!(
            &pending.operation,
            Operation::ControllerRegister { local_id: value } if value == &local_id
        ));
        assert!(!format!("{pending:?}").contains(local_id.as_str()));
        let response = decode(
            machine
                .complete(&pending.key, BackendResult::Refused(Refusal::Unavailable))
                .unwrap(),
        );
        assert_eq!(
            response["result"]["structuredContent"],
            json!({"status":"refused","code":"UNAVAILABLE"})
        );
        assert!(!response.to_string().contains(local_id.as_str()));
        assert!(!response.to_string().contains("private"));
    }
    for bad in [
        String::new(),
        "1".into(),
        "A".repeat(32),
        "g".repeat(32),
        "é".repeat(16),
    ] {
        for tool in ["festerm_controller_status", "festerm_controller_unregister"] {
            tool_error(
                machine.process_value(call(
                    json!(6),
                    tool,
                    json!({"registration_id": bad.as_str()}),
                )),
                "INVALID_ARGUMENTS",
            );
        }
    }
    assert_eq!(machine.in_flight(), 0);
}

#[test]
fn controller_results_are_typed_and_expose_only_correlation_ids() {
    let mut machine = ready_controller();
    let catalog = reply(machine.process_value(request(json!(1), "tools/list", json!({}))));
    let tools = catalog["result"]["tools"].as_array().unwrap();
    let registration = ControllerId::from_random_bytes([0x12; 16]);
    let association = ControllerId::from_random_bytes([0xab; 16]);
    assert_eq!(
        ControllerId::parse(&registration.to_wire()),
        Some(registration)
    );
    assert!(!format!("{registration:?}").contains(&registration.to_wire()));
    let cases = [
        (
            7,
            json!({}),
            BackendResult::ControllerCapabilities {
                enrollment_available: false,
                metadata_available: false,
            },
            json!({
                "enrollment_available":false, "metadata_available":false,
                "metadata_requires_separate_grant":true,
                "presentation_available":false, "input_available":false
            }),
        ),
        (
            8,
            json!({"local_id":"untrusted\nclient"}),
            BackendResult::ControllerStatus {
                registration_id: registration,
                association_id: None,
            },
            json!({"registration_id":registration.to_wire(),"association_id":null}),
        ),
        (
            9,
            json!({"registration_id":registration.to_wire()}),
            BackendResult::ControllerStatus {
                registration_id: registration,
                association_id: Some(association),
            },
            json!({"registration_id":registration.to_wire(),"association_id":association.to_wire()}),
        ),
        (
            10,
            json!({"registration_id":registration.to_wire()}),
            BackendResult::ControllerUnregistered {
                registration_id: registration,
            },
            json!({"registration_id":registration.to_wire(),"removed":true}),
        ),
    ];
    for (index, arguments, result, expected) in cases {
        let name = tools[index]["name"].as_str().unwrap();
        let pending = backend(machine.process_value(call(json!(2), name, arguments)));
        let response = decode(machine.complete(&pending.key, result).unwrap());
        let structured = &response["result"]["structuredContent"];
        assert_eq!(structured, &json!({"status":"ok","data":expected}));
        assert!(schema_matches(&tools[index]["outputSchema"], structured));
        assert!(!response.to_string().contains("untrusted"));
    }

    let wrong = ControllerId::from_random_bytes([0xcd; 16]);
    for (tool, result) in [
        (
            "festerm_controller_status",
            BackendResult::ControllerStatus {
                registration_id: wrong,
                association_id: None,
            },
        ),
        (
            "festerm_controller_unregister",
            BackendResult::ControllerUnregistered {
                registration_id: wrong,
            },
        ),
    ] {
        let pending = backend(machine.process_value(call(
            json!(3),
            tool,
            json!({"registration_id":registration.to_wire()}),
        )));
        let response = decode(machine.complete(&pending.key, result).unwrap());
        assert_eq!(
            response["result"]["structuredContent"]["code"],
            "INVALID_BACKEND_RESULT"
        );
        assert!(!response.to_string().contains(&wrong.to_wire()));
    }
    let pending = backend(machine.process_value(call(
        json!(4),
        "festerm_controller_register",
        json!({"local_id":"client"}),
    )));
    let response = decode(
        machine
            .complete(
                &pending.key,
                BackendResult::AuthorizedObservation(json!({"label":"must-not-leak"})),
            )
            .unwrap(),
    );
    assert_eq!(
        response["result"]["structuredContent"]["code"],
        "INVALID_BACKEND_RESULT"
    );
    assert!(!response.to_string().contains("must-not-leak"));
    let pending =
        backend(machine.process_value(call(json!(5), "festerm_sessions_list", json!({}))));
    let response = decode(
        machine
            .complete(
                &pending.key,
                BackendResult::ControllerStatus {
                    registration_id: registration,
                    association_id: None,
                },
            )
            .unwrap(),
    );
    assert_eq!(
        response["result"]["structuredContent"]["code"],
        "INVALID_BACKEND_RESULT"
    );
    assert!(!response.to_string().contains(&registration.to_wire()));
}

#[test]
fn controller_metadata_capability_requires_backend_and_enrollment_qualification() {
    let mut machine = ready_controller();
    let catalog = reply(machine.process_value(request(json!(1), "tools/list", json!({}))));
    let capability = catalog["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "festerm_controller_capabilities")
        .unwrap();
    let schema = &capability["outputSchema"];
    let metadata_schema =
        &schema["oneOf"][0]["properties"]["data"]["properties"]["metadata_available"];
    assert_eq!(metadata_schema["type"], "boolean");

    for (enrollment_available, metadata_available) in [(true, false), (true, true), (false, true)] {
        let pending = backend(machine.process_value(call(
            json!(2),
            "festerm_controller_capabilities",
            json!({}),
        )));
        let response = decode(
            machine
                .complete(
                    &pending.key,
                    BackendResult::ControllerCapabilities {
                        enrollment_available,
                        metadata_available,
                    },
                )
                .unwrap(),
        );
        assert_eq!(response["result"]["isError"], false);
        let structured = &response["result"]["structuredContent"];
        assert_eq!(
            structured,
            &json!({
                "status":"ok",
                "data":{
                    "enrollment_available":enrollment_available,
                    "metadata_available":enrollment_available && metadata_available,
                    "metadata_requires_separate_grant":true,
                    "presentation_available":false, "input_available":false
                }
            })
        );
        assert!(schema_matches(schema, structured));
    }
}

#[test]
fn controller_requests_share_pending_limits_and_cancellation() {
    let mut machine = ready_controller();
    let mut pending = Vec::new();
    for index in 0..MAX_IN_FLIGHT {
        pending.push(backend(machine.process_value(call(
            json!(index),
            "festerm_controller_register",
            json!({"local_id":format!("client-{index}")}),
        ))));
    }
    error(
        machine.process_value(call(
            json!(MAX_IN_FLIGHT),
            "festerm_controller_capabilities",
            json!({}),
        )),
        -32004,
    );
    let Event::Cancelled(cancelled) = machine.process_value(json!({
        "jsonrpc":"2.0", "method":"notifications/cancelled", "params":{"requestId":0}
    })) else {
        panic!("expected cancellation");
    };
    assert_eq!(cancelled, pending[0].key);
    assert!(machine
        .complete(&cancelled, BackendResult::Refused(Refusal::Unavailable))
        .is_none());
    let replacement = backend(machine.process_value(call(
        json!(MAX_IN_FLIGHT),
        "festerm_controller_capabilities",
        json!({}),
    )));
    assert_eq!(machine.in_flight(), MAX_IN_FLIGHT);
    for request in pending.iter().skip(1).chain(std::iter::once(&replacement)) {
        assert!(machine
            .complete(&request.key, BackendResult::Refused(Refusal::Unavailable))
            .is_some());
    }
    assert_eq!(machine.in_flight(), 0);
}
