//! Bounded MCP 2025-11-25 protocol state machine.
//!
//! This library owns neither transport nor authorization. A connection is not a
//! grant. Client metadata is validated, discarded, and never an identity. The
//! caller must bind each machine to authenticated private IPC, authorize every
//! operation and disclosure, impose deadlines/shared budgets, and cancel backend
//! work on cancellation, disconnect, revocation, or expiry. Keep enablement off
//! by default. [`Machine::new`] is observation-only. An explicitly selected
//! catalog describes later controller registration, but does not implement it.
//! The combined controller/presentation catalog asks the backend for current
//! enrollment availability. The application has a test-qualified consumer for
//! association metadata. An explicitly selected catalog describes direct input
//! without implementing or qualifying it. Production metadata, presentation,
//! and input remain unavailable. No HTTP, workflows, resources, or prompts are
//! exposed.
//!
//! Backend observation data is explicitly authorized **untrusted content**, not
//! instructions. Neither input, output nor client cancellation reasons are logged.
//!
//! # Parent integration
//!
//! * Create one [`Machine`] per enabled stdio connection. Workspace registration,
//!   install-owned transport, native pairing and the default-off setting belong
//!   to the parent application, not this crate.
//! * Feed [`read_bounded_line`] payloads to [`Machine::process_bytes`]. Any
//!   [`LineError`] is fatal: close the transport and call [`Machine::close`].
//! * [`Event::Reply`] contains final wire bytes, including LF. Write them
//!   unchanged to protocol stdout. Do not log them. Bound any caller-owned
//!   outbound queue and close/cancel on output backpressure or write failure.
//! * [`Event::Backend`] carries a [`BackendRequest`] with [`RequestKey`] and
//!   [`Operation`]. Preserve the key through asynchronous work. Bind it to the
//!   trusted connection, never to clientInfo or the JSON-RPC ID as identity.
//! * Resolve opaque session references in the backend; reserve/revalidate
//!   authority, exact targets, recipients, deadlines and shared budgets there.
//!   A read's `max_bytes` caps the **encoded data object**, conservatively counting
//!   metadata as well as text. Session discovery is also a disclosure.
//! * For [`Operation::PairingStatus`], return [`PairingStatus`] with a
//!   [`VerificationCode`] created from a parent-generated random public
//!   fingerprint. Show the same code in native Settings so the user can match
//!   the exact accepted private channel to their MCP client before granting.
//!   Keep it stable for that channel, never reuse it to identify another stream,
//!   and generate it independently of private-channel secrets/opaque principals.
//!   The code is deliberately public, not a bearer credential or principal:
//!   knowing/copying it grants nothing and is useless on another stream.
//!   Neither connection nor pairing-status calls trigger native approval.
//! * After the backend's final disclosure check, call [`Machine::complete`].
//!   Only [`BackendResult::AuthorizedObservation`] releases observation data.
//!   Success has `structuredContent: {"status":"ok","data":{...}}`, with the same
//!   serialized object in one text content block. Refusals have
//!   `{"status":"refused","code":"..."}` and `isError: true`. Data semantics
//!   (revision, absent selection, truncation/gap, wait outcome) belong to the
//!   backend; no completion status or terminal content is invented here.
//! * For direct input, return [`BackendResult::Refused`] only when admission
//!   definitely did not occur. [`InputDisposition::Admitted`] means queue
//!   admission, not execution; use [`InputDisposition::OutcomeUnknown`] when
//!   publication may have happened. Cancellation or lost response bytes do not
//!   prove non-admission. Never automatically replay an uncertain action.
//! * On [`Event::Cancelled`], cancel that exact backend key without replying.
//!   [`Machine::close`] returns all remaining keys (at most four); cancel those
//!   on EOF/disconnect, revocation, lock, expiry or fatal framing failure.
//!   Drop unsent responses when authority expires. Late completions return None.
//! * The machine retains no completed results or unbounded replay ledger.
//!   Concurrent duplicate IDs refuse; a reused completed/cancelled wire ID gets
//!   a distinct process-local generation, never a transferable grant.
//!
//! # Compatibility and ceilings
//!
//! The only negotiated baseline is [`PROTOCOL_VERSION`] (`2025-11-25`).
//! Other proposed versions receive that supported version, per MCP negotiation;
//! clients which cannot use it must disconnect. No "latest" claim is made.
//! IDs preserve strings and signed/unsigned 64-bit integers exactly, including
//! integers above 2^53. Floats/exponents, null and out-of-range integers refuse.
//! Inputs have 64 KiB payload, depth 16, 128 members per collection, 2,048 total
//! values, 32 KiB string/key and 256-byte ID ceilings. References/cursors have
//! 256-Unicode-scalar ceilings. Reads default/cap at 32 KiB and waits at 30 s;
//! zero/over-limit arguments refuse, rather than silently clamp. All output,
//! including LF and the compatibility wrapper, fits in 64 KiB or is wholly
//! replaced by a content-free refusal. The line reader consumes at most
//! 64 KiB + 1, counting delimiters; CRLF therefore costs one extra byte.

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{json, Map, Number, Value};
use std::fmt;
use std::io::{self, BufRead, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};

mod direct_input;
pub mod identity;
mod presentation;
mod schema;
pub mod secure_channel;
pub use secure_channel::handshake_io as secure_handshake_io;
pub mod secure_record_io;
pub mod secure_transport;

pub const PROTOCOL_VERSION: &str = "2025-11-25";
pub const MAX_MESSAGE_BYTES: usize = 65_536;
pub const MAX_READ_BYTES: u32 = 32_768;
pub const MAX_WAIT_MS: u32 = 30_000;
pub const MAX_IN_FLIGHT: usize = 4;
pub const MAX_DEPTH: usize = 16;
pub const MAX_COLLECTION_ITEMS: usize = 128;
pub const MAX_STRING_BYTES: usize = 32_768;
pub const MAX_VALUE_NODES: usize = 2_048;
pub const MAX_REFERENCE_CHARS: usize = 256;
pub const MAX_ID_BYTES: usize = 256;
pub const VERIFICATION_CODE_LEN: usize = 16;
pub const MAX_CONTROLLER_LOCAL_ID_BYTES: usize = 128;
pub const MAX_DIRECT_INPUT_BYTES: usize = 16_384;
pub const MAX_DIRECT_INPUT_KEYS: usize = 64;
pub const MAX_DIRECT_INPUT_KEY_BYTES: usize = 64;

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Exact string or signed/unsigned 64-bit integer ID. Floating-point IDs,
/// including integer-looking floats/exponents, are refused rather than rounded.
#[derive(Clone, PartialEq, Eq)]
pub struct RequestId(Value);

impl RequestId {
    pub fn as_value(&self) -> &Value {
        &self.0
    }

    fn parse(value: &Value) -> Option<Self> {
        match value {
            Value::String(s) if s.len() <= MAX_ID_BYTES => Some(Self(value.clone())),
            Value::Number(n) if n.is_i64() || n.is_u64() => Some(Self(value.clone())),
            _ => None,
        }
    }
}

impl fmt::Debug for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RequestId(<redacted>)")
    }
}

/// The generation prevents a cancelled/completed request's late callback from
/// completing a newer request with the same wire ID.
#[derive(Clone, PartialEq, Eq)]
pub struct RequestKey {
    id: RequestId,
    generation: u64,
}

impl RequestKey {
    pub fn id(&self) -> &RequestId {
        &self.id
    }
}

impl fmt::Debug for RequestKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RequestKey(<redacted>)")
    }
}

/// A connection-local correlation value, not a target, credential, or grant.
///
/// The application must generate independent random bytes, ensure uniqueness,
/// and bind the identifier to the originating connection. This type validates
/// encoding only; it creates neither an association nor authority.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ControllerId([u8; 16]);

impl ControllerId {
    pub fn from_random_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub fn parse(value: &str) -> Option<Self> {
        if value.len() != 32
            || !value
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return None;
        }
        let mut bytes = [0; 16];
        for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
            let nibble = |byte: u8| match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                _ => unreachable!("hex was checked"),
            };
            bytes[index] = (nibble(pair[0]) << 4) | nibble(pair[1]);
        }
        Some(Self(bytes))
    }

    pub fn to_wire(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = String::with_capacity(32);
        for byte in self.0 {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        encoded
    }
}

impl fmt::Debug for ControllerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ControllerId(<redacted>)")
    }
}

/// Session references and scrollback cursors are opaque, not validated targets
/// or bearer credentials. The backend must resolve and authorize them anew.
#[derive(Clone, PartialEq, Eq)]
pub enum Operation {
    PairingStatus,
    ControllerCapabilities,
    PresentationCapabilities,
    ControllerRegister {
        local_id: String,
    },
    ControllerStatus {
        registration_id: ControllerId,
    },
    ControllerUnregister {
        registration_id: ControllerId,
    },
    SessionsList,
    SessionsGet {
        session_ref: String,
    },
    TerminalReadScreen {
        session_ref: String,
        max_bytes: u32,
    },
    TerminalReadScrollback {
        session_ref: String,
        cursor: Option<String>,
        max_bytes: u32,
    },
    TerminalGetSelection {
        session_ref: String,
        max_bytes: u32,
    },
    TerminalWaitForChange {
        session_ref: String,
        after_revision: u64,
        timeout_ms: u32,
    },
    TerminalSendText {
        enrollment_ref: String,
        text: String,
        submit: bool,
        presentation_revision: Option<String>,
    },
    TerminalSendKeys {
        enrollment_ref: String,
        keys: Vec<String>,
        presentation_revision: Option<String>,
    },
}

impl fmt::Debug for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Operation(<redacted>)")
    }
}

impl Operation {
    fn is_observation(&self) -> bool {
        matches!(
            self,
            Self::SessionsList
                | Self::SessionsGet { .. }
                | Self::TerminalReadScreen { .. }
                | Self::TerminalReadScrollback { .. }
                | Self::TerminalGetSelection { .. }
                | Self::TerminalWaitForChange { .. }
        )
    }

    fn result_limit(&self) -> usize {
        match self {
            Self::TerminalReadScreen { max_bytes, .. }
            | Self::TerminalReadScrollback { max_bytes, .. }
            | Self::TerminalGetSelection { max_bytes, .. } => *max_bytes as usize,
            _ => MAX_READ_BYTES as usize,
        }
    }
}

#[derive(Debug)]
pub struct BackendRequest {
    pub key: RequestKey,
    pub operation: Operation,
}

/// A public channel-matching fingerprint, never a secret, principal or grant.
/// The parent generates independent random codes and binds each to exactly one
/// accepted private channel; this type validates format only, not provenance or
/// uniqueness. It never authenticates or authorizes a caller.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VerificationCode([u8; VERIFICATION_CODE_LEN]);

impl VerificationCode {
    /// Accept exactly 16 lowercase ASCII hex characters. No normalization,
    /// arbitrary backend text, whitespace, control characters or prefixes.
    pub fn new(value: &str) -> Option<Self> {
        let bytes: [u8; VERIFICATION_CODE_LEN] = value.as_bytes().try_into().ok()?;
        bytes
            .iter()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            .then_some(Self(bytes))
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("verification code contains only ASCII hex")
    }
}

impl fmt::Debug for VerificationCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VerificationCode(<public fingerprint>)")
    }
}

/// Bootstrap metadata only: no grant, opaque principal, secret, target or
/// terminal data. The public verification code is for explicit native matching,
/// not authentication, automatic approval or a transferable capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PairingStatus {
    pub paired: bool,
    pub observation_enabled: bool,
    pub verification_code: VerificationCode,
}

/// Fixed, content-free refusals. Raw exceptions/diagnostics have no wire path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    NotAuthorized,
    StaleTarget,
    Closed,
    BudgetExceeded,
    InvalidArguments,
    InvalidBackendResult,
    ResponseTooLarge,
    Unavailable,
}

impl Refusal {
    pub(crate) const ALL: [Self; 8] = [
        Self::NotAuthorized,
        Self::StaleTarget,
        Self::Closed,
        Self::BudgetExceeded,
        Self::InvalidArguments,
        Self::InvalidBackendResult,
        Self::ResponseTooLarge,
        Self::Unavailable,
    ];

    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::NotAuthorized => "NOT_AUTHORIZED",
            Self::StaleTarget => "STALE_TARGET",
            Self::Closed => "CLOSED",
            Self::BudgetExceeded => "BUDGET_EXCEEDED",
            Self::InvalidArguments => "INVALID_ARGUMENTS",
            Self::InvalidBackendResult => "INVALID_BACKEND_RESULT",
            Self::ResponseTooLarge => "RESPONSE_TOO_LARGE",
            Self::Unavailable => "UNAVAILABLE",
        }
    }
}

pub enum BackendResult {
    PairingStatus(PairingStatus),
    /// Backend-qualified feature availability, not authorization or grant status.
    ControllerCapabilities {
        enrollment_available: bool,
        metadata_available: bool,
        input_available: bool,
    },
    /// Only the submitting connection's non-authorizing correlation identifiers.
    ControllerStatus {
        registration_id: ControllerId,
        association_id: Option<ControllerId>,
    },
    ControllerUnregistered {
        registration_id: ControllerId,
    },
    /// The backend has authorized release to this connection at completion.
    /// Must be an object; all fields remain untrusted data. Do not pass a raw
    /// backend error here. Reads count encoded data bytes against max_bytes.
    AuthorizedObservation(Value),
    /// A direct input's admission disposition, never shell completion.
    InputDisposition(InputDisposition),
    /// A refusal must mean the input was definitely not admitted.
    Refused(Refusal),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputDisposition {
    Admitted,
    OutcomeUnknown,
}

impl InputDisposition {
    fn as_str(self) -> &'static str {
        match self {
            Self::Admitted => "admitted",
            Self::OutcomeUnknown => "outcome_unknown",
        }
    }
}

/// Already encoded, single-line JSON-RPC plus LF, at most 64 KiB including LF.
/// Deliberately has no Debug/Display implementation that could log content.
pub struct Response(Vec<u8>);

impl Response {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

pub enum Event {
    Reply(Response),
    Backend(BackendRequest),
    /// No response is sent for the cancelled request. Cancel backend work now.
    Cancelled(RequestKey),
    /// Valid/ignored notification; never dispatches a tools/call notification.
    NoReply,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifecycle {
    New,
    AwaitingInitialized,
    Ready,
    Closed,
}

struct Pending {
    key: RequestKey,
    operation: Operation,
}

pub struct Machine {
    lifecycle: Lifecycle,
    pending: Vec<Pending>,
    unavailable_presentation: bool,
    controller_enrollment: bool,
    direct_input: bool,
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine {
    pub fn new() -> Self {
        Self {
            lifecycle: Lifecycle::New,
            pending: Vec::with_capacity(MAX_IN_FLIGHT),
            unavailable_presentation: false,
            controller_enrollment: false,
            direct_input: false,
        }
    }

    /// Later-stage capability negotiation with an explicitly unavailable
    /// presentation operation. No native decision or activation can be made
    /// through this mode. It does not enable the separate enrollment catalog;
    /// `new()` retains the observation-only P1 catalog.
    pub fn with_unavailable_presentation() -> Self {
        Self {
            unavailable_presentation: true,
            ..Self::new()
        }
    }

    /// Selects the controller schema for an explicitly opted-in host.
    /// This does not register clients, open prompts, qualify private bootstrap
    /// or enable presentation.
    pub fn with_controller_enrollment() -> Self {
        Self {
            controller_enrollment: true,
            ..Self::new()
        }
    }

    /// Selects both later-stage catalogs for an explicitly opted-in host.
    /// Presentation remains refusal-only, and its enrollment availability is
    /// requested from the backend rather than inferred from catalog selection.
    pub fn with_controller_enrollment_and_unavailable_presentation() -> Self {
        Self {
            controller_enrollment: true,
            ..Self::with_unavailable_presentation()
        }
    }

    /// Selects the direct-input schemas for an explicitly qualified host. This
    /// catalog is not a grant, native decision, platform qualification or
    /// permission to replay input. Existing constructors remain unchanged. The
    /// application must keep this catalog disabled until its separate input
    /// and delivery qualifications are satisfied.
    pub fn with_direct_input_catalog() -> Self {
        Self {
            direct_input: true,
            ..Self::with_controller_enrollment_and_unavailable_presentation()
        }
    }

    pub fn lifecycle(&self) -> Lifecycle {
        self.lifecycle
    }

    pub fn in_flight(&self) -> usize {
        self.pending.len()
    }

    /// Process one UTF-8 JSON payload, optionally terminated by LF/CRLF.
    /// Literal embedded line breaks, batches, duplicate object keys and
    /// oversized/deep/large-collection values are refused before dispatch.
    pub fn process_bytes(&mut self, bytes: &[u8]) -> Event {
        let payload = bytes.strip_suffix(b"\n").unwrap_or(bytes);
        let payload = if bytes.ends_with(b"\n") {
            payload.strip_suffix(b"\r").unwrap_or(payload)
        } else {
            payload
        };
        if payload.len() > MAX_MESSAGE_BYTES {
            return self.error(None, ProtocolError::LimitExceeded);
        }
        if payload.iter().any(|b| matches!(b, b'\n' | b'\r')) {
            return self.error(None, ProtocolError::Parse);
        }
        let mut deserializer = serde_json::Deserializer::from_slice(payload);
        let mut nodes = 0;
        let value = ValueSeed {
            depth: 0,
            nodes: &mut nodes,
        }
        .deserialize(&mut deserializer);
        match value {
            Ok(value) if deserializer.end().is_ok() => self.process_checked(value),
            _ => self.error(None, ProtocolError::Parse),
        }
    }

    /// Convenience for already parsed values. The caller owns allocation and
    /// duplicate-key checking before this boundary; prefer process_bytes on wire
    /// input. All structural and encoded-size limits are still enforced here.
    pub fn process_value(&mut self, value: Value) -> Event {
        if !bounded_value(&value, 0, &mut 0) || encode_json(&value, MAX_MESSAGE_BYTES).is_none() {
            return self.error(None, ProtocolError::LimitExceeded);
        }
        self.process_checked(value)
    }

    fn process_checked(&mut self, value: Value) -> Event {
        let Some(object) = value.as_object() else {
            return self.error(None, ProtocolError::InvalidRequest);
        };
        let method = object.get("method").and_then(Value::as_str);
        let notification = !object.contains_key("id") && method.is_some();
        let id = object.get("id").and_then(RequestId::parse);
        if object.get("jsonrpc") != Some(&json!("2.0"))
            || method.is_none()
            || (object.contains_key("id") && id.is_none())
            || !schema::only_keys(object, &["jsonrpc", "id", "method", "params"])
        {
            return if notification {
                Event::NoReply
            } else {
                self.error(None, ProtocolError::InvalidRequest)
            };
        }
        let method = method.expect("checked method");
        let params = object.get("params");
        if notification {
            return self.notification(method, params);
        }
        let id = id.expect("request ID checked");
        if self.pending.iter().any(|p| p.key.id == id) {
            return self.error(Some(&id), ProtocolError::DuplicateId);
        }
        if self.lifecycle == Lifecycle::Closed {
            return self.error(Some(&id), ProtocolError::Closed);
        }
        if method == "ping" {
            return if schema::empty_params(params) {
                Event::Reply(success(&id, json!({})))
            } else {
                self.error(Some(&id), ProtocolError::InvalidParams)
            };
        }
        if method == "initialize" {
            if self.lifecycle != Lifecycle::New {
                return self.error(Some(&id), ProtocolError::InvalidRequest);
            }
            if !schema::initialize(params) {
                return self.error(Some(&id), ProtocolError::InvalidParams);
            }
            // Version negotiation always selects our one supported baseline,
            // never adopts a client-supplied version or claims to be "latest".
            self.lifecycle = Lifecycle::AwaitingInitialized;
            return Event::Reply(success(
                &id,
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {
                        "name": "festerm-observation",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            ));
        }
        if self.lifecycle != Lifecycle::Ready {
            return self.error(Some(&id), ProtocolError::NotReady);
        }
        match method {
            "tools/list" => {
                if !schema::empty_params(params) {
                    return self.error(Some(&id), ProtocolError::InvalidParams);
                }
                let mut tools = schema::tools(self.controller_enrollment, self.direct_input);
                if self.unavailable_presentation {
                    tools.extend(presentation::tools(self.controller_enrollment));
                }
                if self.direct_input {
                    tools.extend(direct_input::tools());
                }
                Event::Reply(success(&id, json!({"tools": tools})))
            }
            "tools/call" => {
                let presentation = if self.unavailable_presentation {
                    presentation::call(params)
                } else {
                    None
                };
                let operation = match presentation {
                    Some(Ok(presentation::Call::Capabilities)) if self.controller_enrollment => {
                        Operation::PresentationCapabilities
                    }
                    Some(Ok(presentation::Call::Capabilities)) => {
                        return Event::Reply(tool_result(
                            &id,
                            json!({"status": "ok", "data": presentation::unavailable(false, false)}),
                            false,
                        ));
                    }
                    Some(Ok(presentation::Call::Present)) => {
                        return Event::Reply(tool_refusal(&id, Refusal::Unavailable));
                    }
                    Some(Err(schema::CallError::Protocol)) => {
                        return self.error(Some(&id), ProtocolError::InvalidParams);
                    }
                    Some(Err(schema::CallError::Arguments)) => {
                        return Event::Reply(tool_refusal(&id, Refusal::InvalidArguments));
                    }
                    None => {
                        let direct_input = if self.direct_input {
                            direct_input::call(params)
                        } else {
                            None
                        };
                        let parsed = direct_input
                            .unwrap_or_else(|| schema::call(params, self.controller_enrollment));
                        match parsed {
                            Ok(operation) => operation,
                            Err(schema::CallError::Protocol) => {
                                return self.error(Some(&id), ProtocolError::InvalidParams);
                            }
                            Err(schema::CallError::Arguments) => {
                                return Event::Reply(tool_refusal(&id, Refusal::InvalidArguments));
                            }
                        }
                    }
                };
                if self.pending.len() >= MAX_IN_FLIGHT {
                    return self.error(Some(&id), ProtocolError::Busy);
                }
                let Ok(generation) =
                    NEXT_GENERATION.try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                        value.checked_add(1)
                    })
                else {
                    return self.error(Some(&id), ProtocolError::Busy);
                };
                let key = RequestKey { id, generation };
                self.pending.push(Pending {
                    key: key.clone(),
                    operation: operation.clone(),
                });
                Event::Backend(BackendRequest { key, operation })
            }
            _ => self.error(Some(&id), ProtocolError::MethodNotFound),
        }
    }

    fn notification(&mut self, method: &str, params: Option<&Value>) -> Event {
        match method {
            "notifications/initialized"
                if self.lifecycle == Lifecycle::AwaitingInitialized
                    && schema::empty_params(params) =>
            {
                self.lifecycle = Lifecycle::Ready;
            }
            "notifications/cancelled" if self.lifecycle == Lifecycle::Ready => {
                if let Some(id) = schema::cancelled(params) {
                    if let Some(index) = self.pending.iter().position(|p| p.key.id == id) {
                        return Event::Cancelled(self.pending.remove(index).key);
                    }
                }
            }
            _ => {}
        }
        Event::NoReply
    }

    /// Finalizes exactly one live generation and immediately frees capacity.
    /// Late/duplicate completions return None. Results are not retained/queued.
    /// The parent must revalidate grant/recipient/epochs immediately before this
    /// call and drop unsent responses when authorization expires.
    pub fn complete(&mut self, key: &RequestKey, result: BackendResult) -> Option<Response> {
        let index = self.pending.iter().position(|p| p.key == *key)?;
        let pending = self.pending.remove(index);
        let data = match (&pending.operation, result) {
            (_, BackendResult::Refused(reason)) => return Some(tool_refusal(&key.id, reason)),
            (Operation::PairingStatus, BackendResult::PairingStatus(status)) => json!({
                "paired": status.paired,
                "observation_enabled": status.observation_enabled,
                "verification_code": status.verification_code.as_str()
            }),
            (
                Operation::ControllerCapabilities,
                BackendResult::ControllerCapabilities {
                    enrollment_available,
                    metadata_available,
                    input_available,
                },
            ) => json!({
                "enrollment_available": enrollment_available,
                "metadata_available": enrollment_available && metadata_available,
                "metadata_requires_separate_grant": true,
                "presentation_available": false,
                "input_available": self.direct_input && enrollment_available && input_available,
            }),
            (
                Operation::PresentationCapabilities,
                BackendResult::ControllerCapabilities {
                    enrollment_available,
                    metadata_available: _,
                    input_available,
                },
            ) => presentation::unavailable(
                enrollment_available,
                self.direct_input && enrollment_available && input_available,
            ),
            (
                Operation::ControllerRegister { .. },
                BackendResult::ControllerStatus {
                    registration_id,
                    association_id,
                },
            ) => json!({
                "registration_id": registration_id.to_wire(),
                "association_id": association_id.map(ControllerId::to_wire),
            }),
            (
                Operation::ControllerStatus {
                    registration_id: expected,
                },
                BackendResult::ControllerStatus {
                    registration_id,
                    association_id,
                },
            ) if expected == &registration_id => json!({
                "registration_id": registration_id.to_wire(),
                "association_id": association_id.map(ControllerId::to_wire),
            }),
            (
                Operation::ControllerUnregister {
                    registration_id: expected,
                },
                BackendResult::ControllerUnregistered { registration_id },
            ) if expected == &registration_id => {
                json!({"registration_id": registration_id.to_wire(), "removed": true})
            }
            (operation, BackendResult::AuthorizedObservation(data))
                if operation.is_observation() =>
            {
                data
            }
            (
                Operation::TerminalSendText { .. } | Operation::TerminalSendKeys { .. },
                BackendResult::InputDisposition(disposition),
            ) => {
                let unknown = disposition == InputDisposition::OutcomeUnknown;
                let status = if unknown { "unknown" } else { "ok" };
                return Some(tool_result(
                    &key.id,
                    json!({"status": status, "data": {"disposition": disposition.as_str()}}),
                    unknown,
                ));
            }
            (Operation::TerminalSendText { .. } | Operation::TerminalSendKeys { .. }, _) => {
                return Some(tool_result(
                    &key.id,
                    json!({"status": "unknown", "data": {"disposition": "outcome_unknown"}}),
                    true,
                ));
            }
            _ => return Some(tool_refusal(&key.id, Refusal::InvalidBackendResult)),
        };
        if !data.is_object() {
            return Some(tool_refusal(&key.id, Refusal::InvalidBackendResult));
        }
        if !bounded_value(&data, 0, &mut 0)
            || encode_json(&data, pending.operation.result_limit()).is_none()
        {
            return Some(tool_refusal(&key.id, Refusal::ResponseTooLarge));
        }
        let structured = json!({"status": "ok", "data": data});
        Some(tool_result(&key.id, structured, false))
    }

    /// Close permanently on EOF, framing failure, revocation, or transport loss.
    /// Cancel each returned backend key. There are never more than four.
    pub fn close(&mut self) -> Vec<RequestKey> {
        self.lifecycle = Lifecycle::Closed;
        self.pending.drain(..).map(|p| p.key).collect()
    }

    fn error(&self, id: Option<&RequestId>, error: ProtocolError) -> Event {
        Event::Reply(protocol_error(id, error))
    }
}

#[derive(Clone, Copy)]
enum ProtocolError {
    Parse,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    NotReady,
    DuplicateId,
    Busy,
    Closed,
    LimitExceeded,
}

fn protocol_error(id: Option<&RequestId>, error: ProtocolError) -> Response {
    let (code, message) = match error {
        ProtocolError::Parse => (-32700, "Invalid JSON payload"),
        ProtocolError::InvalidRequest => (-32600, "Invalid request"),
        ProtocolError::MethodNotFound => (-32601, "Method not found"),
        ProtocolError::InvalidParams => (-32602, "Invalid parameters"),
        ProtocolError::NotReady => (-32002, "Initialization required"),
        ProtocolError::DuplicateId => (-32003, "Duplicate in-flight request ID"),
        ProtocolError::Busy => (-32004, "Request capacity exceeded"),
        ProtocolError::Closed => (-32005, "Connection closed"),
        ProtocolError::LimitExceeded => (-32006, "Payload limit exceeded"),
    };
    response(&json!({
        "jsonrpc": "2.0",
        "id": id.map(RequestId::as_value),
        "error": {"code": code, "message": message}
    }))
    .expect("fixed protocol error and bounded ID fit")
}

fn success(id: &RequestId, result: Value) -> Response {
    response(&json!({"jsonrpc": "2.0", "id": id.as_value(), "result": result}))
        .unwrap_or_else(|| protocol_error(Some(id), ProtocolError::LimitExceeded))
}

fn tool_refusal(id: &RequestId, reason: Refusal) -> Response {
    tool_result(
        id,
        json!({"status": "refused", "code": reason.code()}),
        true,
    )
}

fn tool_result(id: &RequestId, structured: Value, is_error: bool) -> Response {
    let text =
        encode_json(&structured, MAX_MESSAGE_BYTES).and_then(|bytes| String::from_utf8(bytes).ok());
    if let Some(text) = text {
        if let Some(encoded) = response(&json!({
            "jsonrpc": "2.0",
            "id": id.as_value(),
            "result": {
                "structuredContent": structured,
                "content": [{"type": "text", "text": text}],
                "isError": is_error
            }
        })) {
            return encoded;
        }
    }
    // Both the structured content and the compatibility wrapper count toward
    // the ceiling. Refuse atomically; never truncate or echo backend fragments.
    tool_refusal(id, Refusal::ResponseTooLarge)
}

fn response(value: &Value) -> Option<Response> {
    if !bounded_value(value, 0, &mut 0) {
        return None;
    }
    let mut bytes = encode_json(value, MAX_MESSAGE_BYTES - 1)?;
    bytes.push(b'\n');
    Some(Response(bytes))
}

struct LimitedWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::ErrorKind::FileTooLarge.into());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_json(value: &Value, limit: usize) -> Option<Vec<u8>> {
    let mut writer = LimitedWriter {
        bytes: Vec::with_capacity(limit.min(1_024)),
        limit,
    };
    serde_json::to_writer(&mut writer, value).ok()?;
    Some(writer.bytes)
}

/// Framing errors are fatal: close the transport, do not parse the unread suffix
/// of an oversized line as another message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineError {
    TooLong,
    Unterminated,
    Io(io::ErrorKind),
}

/// Consumes at most MAX_MESSAGE_BYTES + 1 bytes, including LF, per call.
/// Returns a payload without LF (and optional CR). EOF without LF is refused.
pub fn read_bounded_line<R: BufRead>(reader: &mut R) -> Result<Option<Vec<u8>>, LineError> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_MESSAGE_BYTES + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .map_err(|error| LineError::Io(error.kind()))?;
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.last() != Some(&b'\n') {
        return Err(if bytes.len() > MAX_MESSAGE_BYTES {
            LineError::TooLong
        } else {
            LineError::Unterminated
        });
    }
    bytes.pop();
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    Ok(Some(bytes))
}

fn bounded_value(value: &Value, depth: usize, nodes: &mut usize) -> bool {
    *nodes += 1;
    if depth > MAX_DEPTH || *nodes > MAX_VALUE_NODES {
        return false;
    }
    match value {
        Value::String(s) => s.len() <= MAX_STRING_BYTES,
        Value::Array(values) => {
            values.len() <= MAX_COLLECTION_ITEMS
                && values.iter().all(|v| bounded_value(v, depth + 1, nodes))
        }
        Value::Object(values) => {
            values.len() <= MAX_COLLECTION_ITEMS
                && values
                    .iter()
                    .all(|(k, v)| k.len() <= MAX_STRING_BYTES && bounded_value(v, depth + 1, nodes))
        }
        _ => true,
    }
}

// Deserialize with bounds before descending/allocating collections, and refuse
// duplicate keys rather than letting serde_json::Value silently overwrite them.
struct ValueSeed<'a> {
    depth: usize,
    nodes: &'a mut usize,
}

impl<'de> DeserializeSeed<'de> for ValueSeed<'_> {
    type Value = Value;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        *self.nodes += 1;
        if self.depth > MAX_DEPTH || *self.nodes > MAX_VALUE_NODES {
            return Err(de::Error::custom("Value limit exceeded"));
        }
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for ValueSeed<'_> {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("bounded JSON value")
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("Invalid number"))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        if value.len() > MAX_STRING_BYTES {
            return Err(E::custom("String limit exceeded"));
        }
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Value, E> {
        if value.len() > MAX_STRING_BYTES {
            return Err(E::custom("String limit exceeded"));
        }
        Ok(Value::String(value))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        loop {
            // next_element_seed checks depth/node limits before construction.
            let next = seq.next_element_seed(ValueSeed {
                depth: self.depth + 1,
                nodes: self.nodes,
            })?;
            let Some(value) = next else { break };
            if values.len() == MAX_COLLECTION_ITEMS {
                return Err(de::Error::custom("Collection limit exceeded"));
            }
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if key.len() > MAX_STRING_BYTES
                || values.len() == MAX_COLLECTION_ITEMS
                || values.contains_key(&key)
            {
                return Err(de::Error::custom("Invalid object member"));
            }
            let value = map.next_value_seed(ValueSeed {
                depth: self.depth + 1,
                nodes: self.nodes,
            })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}
