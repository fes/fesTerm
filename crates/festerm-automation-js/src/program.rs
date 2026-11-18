//! Bounded v1 contracts and an in-process runner for terminal programs.
//!
//! This module deliberately does not provide a production worker, a process
//! launcher, or any automation/session authority. The runner is a test seam
//! for exercising terminal I/O contracts before native process integration.

use crate::{
    protocol::{encode_frame, CHUNK_BYTES, MAX_FRAME_BYTES, MAX_SAFE_INTEGER},
    snapshot::{token, unique_map, ApprovedBundle, Manifest, SourceSnapshot, MAX_SNAPSHOT_BYTES},
    Error,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const PROGRAM_PROTOCOL_VERSION: u16 = 1;
pub const PROGRAM_API_VERSION: u16 = 1;
pub const MAX_INPUT_CHUNK_BYTES: usize = CHUNK_BYTES;
pub const MAX_OUTPUT_CHUNK_BYTES: usize = CHUNK_BYTES;
pub const MAX_OUTPUT_QUEUE_BYTES: usize = 32 * 1024;
pub const MAX_PENDING_WRITES: usize = 16;
pub const MAX_PENDING_EVENTS: usize = 16;
pub const MAX_PENDING_FRAMES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProgramKind {
    TerminalProgram,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProgramCapability {
    TerminalInput,
    TerminalOutput,
    TerminalResize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProgramManifest {
    pub version: u16,
    pub api_version: u16,
    pub kind: ProgramKind,
    pub name: String,
    pub entry: String,
    /// Exact sorted canonical paths, equal to the supplied module keys.
    pub modules: Vec<String>,
    pub capabilities: BTreeSet<ProgramCapability>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramSourceSnapshot {
    pub manifest: ProgramManifest,
    #[serde(deserialize_with = "unique_map")]
    pub modules: BTreeMap<String, String>,
}

/// An immutable validated source set. Only in-memory bytes are accepted.
#[derive(Clone)]
pub struct ApprovedProgram {
    snapshot: ProgramSourceSnapshot,
    digest: String,
}

impl std::fmt::Debug for ApprovedProgram {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApprovedProgram")
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

impl ApprovedProgram {
    pub fn new(snapshot: ProgramSourceSnapshot) -> Result<Self, Error> {
        validate_manifest(&snapshot)?;
        ApprovedBundle::new(snapshot_as_workflow(&snapshot))?;
        let canonical = serde_json::to_vec(&snapshot).map_err(|_| Error::Snapshot)?;
        if canonical.len() > MAX_SNAPSHOT_BYTES {
            return Err(Error::Snapshot);
        }
        let digest = format!("{:x}", Sha256::digest(&canonical));
        Ok(Self { snapshot, digest })
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn manifest(&self) -> &ProgramManifest {
        &self.snapshot.manifest
    }

    pub fn snapshot(&self) -> &ProgramSourceSnapshot {
        &self.snapshot
    }

    pub fn snapshot_bytes(&self) -> Result<Vec<u8>, Error> {
        serde_json::to_vec(&self.snapshot).map_err(|_| Error::Snapshot)
    }
}

fn validate_manifest(snapshot: &ProgramSourceSnapshot) -> Result<(), Error> {
    let manifest = &snapshot.manifest;
    if manifest.version != PROGRAM_PROTOCOL_VERSION
        || manifest.api_version != PROGRAM_API_VERSION
        || manifest.kind != ProgramKind::TerminalProgram
        || !token(&manifest.name)
        || manifest.capabilities.is_empty()
        || manifest.capabilities.len() > 3
    {
        return Err(Error::Snapshot);
    }
    Ok(())
}

fn snapshot_as_workflow(snapshot: &ProgramSourceSnapshot) -> SourceSnapshot {
    SourceSnapshot {
        manifest: Manifest {
            version: 1,
            api_version: 1,
            name: snapshot.manifest.name.clone(),
            entry: snapshot.manifest.entry.clone(),
            modules: snapshot.manifest.modules.clone(),
            parameters: BTreeMap::new(),
            capabilities: BTreeSet::new(),
        },
        modules: snapshot.modules.clone(),
    }
}

/// Incrementally reconstructs an immutable bounded program snapshot.
pub struct ProgramBundleLoader {
    digest: String,
    expected_bytes: usize,
    source: Vec<u8>,
}

impl ProgramBundleLoader {
    pub fn begin(digest: String, expected_bytes: usize) -> Result<Self, Error> {
        if digest.len() != 64
            || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            || expected_bytes == 0
            || expected_bytes > MAX_SNAPSHOT_BYTES
        {
            return Err(Error::Protocol);
        }
        Ok(Self {
            digest,
            expected_bytes,
            source: Vec::with_capacity(expected_bytes),
        })
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if bytes.is_empty()
            || bytes.len() > CHUNK_BYTES
            || self.source.len() + bytes.len() > self.expected_bytes
        {
            return Err(Error::Protocol);
        }
        self.source.extend_from_slice(bytes);
        Ok(())
    }

    pub fn remaining(&self) -> usize {
        self.expected_bytes - self.source.len()
    }

    pub fn finish(self) -> Result<ApprovedProgram, Error> {
        if self.source.len() != self.expected_bytes {
            return Err(Error::Protocol);
        }
        let snapshot: ProgramSourceSnapshot =
            serde_json::from_slice(&self.source).map_err(|_| Error::Snapshot)?;
        let program = ApprovedProgram::new(snapshot)?;
        if program.digest() != self.digest {
            return Err(Error::Snapshot);
        }
        Ok(program)
    }
}

/// Minimal fixed, repository-owned source for deterministic in-process tests.
pub fn developer_echo_program() -> Result<ApprovedProgram, Error> {
    ApprovedProgram::new(ProgramSourceSnapshot {
        manifest: ProgramManifest {
            version: PROGRAM_PROTOCOL_VERSION,
            api_version: PROGRAM_API_VERSION,
            kind: ProgramKind::TerminalProgram,
            name: "developer-echo".into(),
            entry: "main.js".into(),
            modules: vec!["main.js".into()],
            capabilities: BTreeSet::from([
                ProgramCapability::TerminalInput,
                ProgramCapability::TerminalOutput,
                ProgramCapability::TerminalResize,
            ]),
        },
        modules: BTreeMap::from([(
            "main.js".into(),
            r#"export function main({terminal}) {
    let pending = [];
    const decode = bytes => {
        pending.push(...bytes);
        let text = "";
        let index = 0;
        while (index < pending.length) {
            const first = pending[index];
            let length = 0;
            if (first <= 0x7f) length = 1;
            else if (first >= 0xc2 && first <= 0xdf) length = 2;
            else if (first >= 0xe0 && first <= 0xef) length = 3;
            else if (first >= 0xf0 && first <= 0xf4) length = 4;
            if (length === 0) {
                text += "\uFFFD";
                index += 1;
                continue;
            }
            if (pending.length - index < length) break;
            const continuation = pending.slice(index + 1, index + length)
                .every(byte => byte >= 0x80 && byte <= 0xbf);
            if (!continuation) {
                text += "\uFFFD";
                index += 1;
                continue;
            }
            let codePoint = length === 1 ? first : first & (0x7f >> length);
            for (let offset = 1; offset < length; offset += 1) {
                codePoint = (codePoint << 6) | (pending[index + offset] & 0x3f);
            }
            if (codePoint <= 0x10ffff && !(codePoint >= 0xd800 && codePoint <= 0xdfff)
                && !(length === 3 && codePoint < 0x0800)
                && !(length === 4 && codePoint < 0x10000)) {
                text += String.fromCodePoint(codePoint);
                index += length;
            } else {
                text += "\uFFFD";
                index += 1;
            }
        }
        pending = pending.slice(index);
        return text;
    };
    terminal.events(event => {
        if (event.type === "input") {
            const text = decode(event.bytes);
            if (text.length !== 0) terminal.write(text);
        } else if (event.type === "resize") {
            terminal.write(`resize:${terminal.size.columns}x${terminal.size.rows}`);
        } else if (event.type === "close") {
            terminal.write("closed");
        }
    });
}"#
            .into(),
        )]),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalSize {
    pub columns: u16,
    pub rows: u16,
}

impl TerminalSize {
    pub fn validate(self) -> Result<(), Error> {
        if self.columns < 2 || self.columns > 1_000 || self.rows == 0 || self.rows > 1_000 {
            return Err(Error::Protocol);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ProgramHostMessage {
    Launch {
        version: u16,
        api_version: u16,
        generation: u64,
        bundle_digest: String,
        initial_size: TerminalSize,
    },
    Input {
        generation: u64,
        sequence: u64,
        bytes: Vec<u8>,
    },
    Resize {
        generation: u64,
        sequence: u64,
        size: TerminalSize,
    },
    Close {
        generation: u64,
        sequence: u64,
    },
    WriteResult {
        generation: u64,
        write_id: u64,
        acceptance: WriteAcceptance,
    },
    Cancel {
        generation: u64,
    },
}

impl ProgramHostMessage {
    pub fn validate(&self) -> Result<(), Error> {
        let safe = |value: &u64| *value > 0 && *value <= MAX_SAFE_INTEGER;
        let valid = match self {
            Self::Launch {
                version,
                api_version,
                generation,
                bundle_digest,
                initial_size,
            } => {
                *version == PROGRAM_PROTOCOL_VERSION
                    && *api_version == PROGRAM_API_VERSION
                    && safe(generation)
                    && bundle_digest.len() == 64
                    && bundle_digest.bytes().all(|b| b.is_ascii_hexdigit())
                    && initial_size.validate().is_ok()
            }
            Self::Input {
                generation,
                sequence,
                bytes,
            } => {
                safe(generation)
                    && safe(sequence)
                    && !bytes.is_empty()
                    && bytes.len() <= MAX_INPUT_CHUNK_BYTES
            }
            Self::Resize {
                generation,
                sequence,
                size,
            } => safe(generation) && safe(sequence) && size.validate().is_ok(),
            Self::Close {
                generation,
                sequence,
            } => safe(generation) && safe(sequence),
            Self::WriteResult {
                generation,
                write_id,
                ..
            } => safe(generation) && safe(write_id),
            Self::Cancel { generation } => safe(generation),
        };
        if valid {
            Ok(())
        } else {
            Err(Error::Protocol)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WriteAcceptance {
    Accepted,
    Backpressure,
    Rejected,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalWrite {
    pub generation: u64,
    pub write_id: u64,
    pub bytes: Vec<u8>,
}

impl TerminalWrite {
    pub fn validate(&self) -> Result<(), Error> {
        if self.generation == 0
            || self.generation > MAX_SAFE_INTEGER
            || self.write_id == 0
            || self.write_id > MAX_SAFE_INTEGER
            || self.bytes.is_empty()
            || self.bytes.len() > MAX_OUTPUT_CHUNK_BYTES
            || std::str::from_utf8(&self.bytes).is_err()
        {
            return Err(Error::Protocol);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProgramFailure {
    Source,
    Protocol,
    ResourceLimit,
    Internal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase", deny_unknown_fields)]
pub enum ProgramOutcome {
    Completed {},
    Failed { failure: ProgramFailure },
    UnsettledWork {},
    Cancelled {},
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ProgramWorkerMessage {
    Ready {
        profile: String,
    },
    Started {
        generation: u64,
    },
    Write {
        generation: u64,
        write_id: u64,
        bytes: Vec<u8>,
    },
    EventCompleted {
        generation: u64,
        sequence: u64,
    },
    Finished {
        generation: u64,
        outcome: ProgramOutcome,
    },
    Failed {
        code: ProgramWorkerFailure,
    },
}

impl ProgramWorkerMessage {
    pub fn validate(&self) -> Result<(), Error> {
        match self {
            Self::Ready { profile } if !profile.is_empty() && profile.len() <= 128 => Ok(()),
            Self::Started { generation } if *generation > 0 && *generation <= MAX_SAFE_INTEGER => {
                Ok(())
            }
            Self::Write {
                generation,
                write_id,
                bytes,
            } => TerminalWrite {
                generation: *generation,
                write_id: *write_id,
                bytes: bytes.clone(),
            }
            .validate(),
            Self::EventCompleted {
                generation,
                sequence,
            } if *generation > 0
                && *generation <= MAX_SAFE_INTEGER
                && *sequence > 0
                && *sequence <= MAX_SAFE_INTEGER =>
            {
                Ok(())
            }
            Self::Finished { generation, .. }
                if *generation > 0 && *generation <= MAX_SAFE_INTEGER =>
            {
                Ok(())
            }
            Self::Failed { .. } => Ok(()),
            Self::Ready { .. }
            | Self::Started { .. }
            | Self::EventCompleted { .. }
            | Self::Finished { .. } => Err(Error::Protocol),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProgramWorkerFailure {
    Unavailable,
    InvalidSnapshot,
    Protocol,
    Io,
    Deadline,
    Runtime,
}

/// Strict bundle-transfer phase for the program worker. It is deliberately
/// separate from the automation workflow's `ParentMessage`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ProgramBundleMessage {
    Begin {
        generation: u64,
        bundle_digest: String,
        snapshot_bytes: u32,
    },
    SourceChunk {
        generation: u64,
        bytes: Vec<u8>,
    },
}

impl ProgramBundleMessage {
    pub fn validate(&self) -> Result<(), Error> {
        let valid_generation = |generation: u64| generation > 0 && generation <= MAX_SAFE_INTEGER;
        let valid = match self {
            Self::Begin {
                generation,
                bundle_digest,
                snapshot_bytes,
            } => {
                valid_generation(*generation)
                    && bundle_digest.len() == 64
                    && bundle_digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                    && *snapshot_bytes > 0
                    && *snapshot_bytes as usize <= MAX_SNAPSHOT_BYTES
            }
            Self::SourceChunk { generation, bytes } => {
                valid_generation(*generation) && !bytes.is_empty() && bytes.len() <= CHUNK_BYTES
            }
        };
        if valid {
            Ok(())
        } else {
            Err(Error::Protocol)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Submission {
    Accepted,
    Backpressure,
    StaleGeneration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProgramEvent {
    Output(Vec<u8>),
    ResizeApplied(TerminalSize),
    Finished(ProgramOutcome),
}

/// Incremental strict v1 frame decoder. Its bounded buffer is intended for
/// nonblocking input adapters and is independent of the production worker.
#[derive(Default)]
pub struct ProgramWireDecoder {
    bytes: Vec<u8>,
    messages: VecDeque<ProgramHostMessage>,
}

impl ProgramWireDecoder {
    pub fn feed(&mut self, bytes: &[u8]) -> Result<usize, Error> {
        if bytes.is_empty() || self.bytes.len() + bytes.len() > MAX_FRAME_BYTES + 4 {
            return Err(Error::Protocol);
        }
        self.bytes.extend_from_slice(bytes);
        let mut decoded = 0;
        loop {
            if self.bytes.len() < 4 {
                break;
            }
            let size = u32::from_be_bytes(self.bytes[..4].try_into().unwrap()) as usize;
            if size == 0 || size > MAX_FRAME_BYTES {
                return Err(Error::Protocol);
            }
            if self.bytes.len() < 4 + size {
                break;
            }
            let message: ProgramHostMessage =
                serde_json::from_slice(&self.bytes[4..4 + size]).map_err(|_| Error::Protocol)?;
            message.validate()?;
            if self.messages.len() >= MAX_PENDING_FRAMES {
                return Err(Error::Protocol);
            }
            self.messages.push_back(message);
            self.bytes.drain(..4 + size);
            decoded += 1;
        }
        Ok(decoded)
    }

    pub fn pop_message(&mut self) -> Option<ProgramHostMessage> {
        self.messages.pop_front()
    }

    pub fn is_idle(&self) -> bool {
        self.bytes.is_empty() && self.messages.is_empty()
    }
}

pub fn encode_program_frame(message: &ProgramHostMessage) -> Result<Vec<u8>, Error> {
    message.validate()?;
    encode_frame(message)
}

pub fn encode_program_worker_frame(message: &ProgramWorkerMessage) -> Result<Vec<u8>, Error> {
    message.validate()?;
    encode_frame(message)
}

/// Deterministic echo/menu test runner. It models ordering and bounded
/// acceptance; it does not execute JavaScript or represent a native worker.
pub struct InProcessProgramRunner {
    program: ApprovedProgram,
    generation: u64,
    next_sequence: u64,
    size: TerminalSize,
    next_write_id: u64,
    pending_writes: VecDeque<(u64, TerminalWrite)>,
    pending_bytes: usize,
    accepted_output: VecDeque<(u64, Vec<u8>)>,
    accepted_output_bytes: usize,
    resize_events: VecDeque<(u64, TerminalSize)>,
    unsettled_host_calls: BTreeSet<u64>,
    outcome: Option<ProgramOutcome>,
}

impl InProcessProgramRunner {
    pub fn launch(
        program: ApprovedProgram,
        generation: u64,
        initial_size: TerminalSize,
    ) -> Result<Self, Error> {
        if generation == 0
            || generation > MAX_SAFE_INTEGER
            || ![
                ProgramCapability::TerminalInput,
                ProgramCapability::TerminalOutput,
                ProgramCapability::TerminalResize,
            ]
            .iter()
            .all(|capability| program.manifest().capabilities.contains(capability))
        {
            return Err(Error::Protocol);
        }
        initial_size.validate()?;
        Ok(Self {
            program,
            generation,
            next_sequence: 1,
            size: initial_size,
            next_write_id: 1,
            pending_writes: VecDeque::new(),
            pending_bytes: 0,
            accepted_output: VecDeque::new(),
            accepted_output_bytes: 0,
            resize_events: VecDeque::new(),
            unsettled_host_calls: BTreeSet::new(),
            outcome: None,
        })
    }

    pub fn program(&self) -> &ApprovedProgram {
        &self.program
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn size(&self) -> TerminalSize {
        self.size
    }

    pub fn submit(&mut self, message: &ProgramHostMessage) -> Result<Submission, Error> {
        message.validate()?;
        if self.outcome.is_some() && !matches!(message, ProgramHostMessage::Cancel { .. }) {
            return Err(Error::Protocol);
        }
        if generation(message) != self.generation {
            return Ok(Submission::StaleGeneration);
        }
        match message {
            ProgramHostMessage::Launch { .. } => {
                let ProgramHostMessage::Launch {
                    bundle_digest,
                    initial_size,
                    ..
                } = message
                else {
                    unreachable!()
                };
                if bundle_digest != self.program.digest() || initial_size != &self.size {
                    Err(Error::Protocol)
                } else {
                    Ok(Submission::Accepted)
                }
            }
            ProgramHostMessage::Input {
                sequence, bytes, ..
            } => {
                self.expect_sequence(*sequence)?;
                if !self.has_write_capacity(bytes.len()) {
                    return Ok(Submission::Backpressure);
                }
                self.next_sequence += 1;
                let _ = self.enqueue_write(*sequence, bytes.clone())?;
                Ok(Submission::Accepted)
            }
            ProgramHostMessage::Resize { sequence, size, .. } => {
                self.expect_sequence(*sequence)?;
                if self.resize_events.len() >= MAX_PENDING_EVENTS {
                    return Ok(Submission::Backpressure);
                }
                self.next_sequence += 1;
                self.size = *size;
                self.resize_events.push_back((*sequence, *size));
                Ok(Submission::Accepted)
            }
            ProgramHostMessage::Close { sequence, .. } => {
                self.expect_sequence(*sequence)?;
                self.next_sequence += 1;
                self.settle()
            }
            ProgramHostMessage::WriteResult {
                write_id,
                acceptance,
                ..
            } => self.resolve_write(*write_id, *acceptance),
            ProgramHostMessage::Cancel { .. } => {
                self.pending_writes.clear();
                self.pending_bytes = 0;
                self.accepted_output.clear();
                self.accepted_output_bytes = 0;
                self.resize_events.clear();
                self.unsettled_host_calls.clear();
                self.outcome = Some(ProgramOutcome::Cancelled {});
                Ok(Submission::Accepted)
            }
        }
    }

    pub fn pending_write(&self) -> Option<&TerminalWrite> {
        self.pending_writes.front().map(|(_, write)| write)
    }

    pub fn write_result(
        &mut self,
        write_id: u64,
        acceptance: WriteAcceptance,
    ) -> Result<Submission, Error> {
        self.submit(&ProgramHostMessage::WriteResult {
            generation: self.generation,
            write_id,
            acceptance,
        })
    }

    pub fn next_event(&mut self) -> Option<ProgramEvent> {
        let output_sequence = self.accepted_output.front().map(|(sequence, _)| *sequence);
        let resize_sequence = self.resize_events.front().map(|(sequence, _)| *sequence);
        let pending_sequence = self.pending_writes.front().map(|(sequence, _)| *sequence);
        if let Some(sequence) = output_sequence.into_iter().chain(resize_sequence).min() {
            if pending_sequence.is_none_or(|pending| sequence < pending) {
                if output_sequence == Some(sequence) {
                    let (_, bytes) = self.accepted_output.pop_front().unwrap();
                    self.accepted_output_bytes -= bytes.len();
                    return Some(ProgramEvent::Output(bytes));
                }
                let (_, size) = self.resize_events.pop_front().unwrap();
                return Some(ProgramEvent::ResizeApplied(size));
            }
        }
        self.outcome.take().map(ProgramEvent::Finished)
    }

    pub fn begin_host_call(&mut self, id: u64) -> Result<(), Error> {
        if id == 0
            || id > MAX_SAFE_INTEGER
            || self.unsettled_host_calls.len() >= MAX_PENDING_WRITES
            || !self.unsettled_host_calls.insert(id)
        {
            return Err(Error::Protocol);
        }
        Ok(())
    }

    pub fn settle_host_call(&mut self, id: u64) -> Result<(), Error> {
        if !self.unsettled_host_calls.remove(&id) {
            return Err(Error::Protocol);
        }
        Ok(())
    }

    pub fn finish(&mut self) -> ProgramOutcome {
        self.settle().unwrap();
        self.outcome.take().expect("settle produced an outcome")
    }

    pub fn fail(&mut self, failure: ProgramFailure) -> ProgramOutcome {
        let outcome = ProgramOutcome::Failed { failure };
        self.outcome = Some(outcome);
        self.pending_writes.clear();
        self.pending_bytes = 0;
        self.accepted_output.clear();
        self.accepted_output_bytes = 0;
        self.resize_events.clear();
        self.unsettled_host_calls.clear();
        outcome
    }

    pub fn accepted_output_bytes(&self) -> usize {
        self.accepted_output_bytes
    }

    fn enqueue_write(&mut self, sequence: u64, bytes: Vec<u8>) -> Result<u64, Error> {
        let write = TerminalWrite {
            generation: self.generation,
            write_id: self.next_write_id,
            bytes,
        };
        write.validate()?;
        if self.pending_writes.len() >= MAX_PENDING_WRITES
            || self.pending_bytes + self.accepted_output_bytes + write.bytes.len()
                > MAX_OUTPUT_QUEUE_BYTES
        {
            return Err(Error::Protocol);
        }
        self.next_write_id += 1;
        self.pending_bytes += write.bytes.len();
        self.pending_writes.push_back((sequence, write));
        Ok(self.next_write_id - 1)
    }

    fn resolve_write(
        &mut self,
        write_id: u64,
        acceptance: WriteAcceptance,
    ) -> Result<Submission, Error> {
        let (_, write) = self.pending_writes.front().ok_or(Error::Protocol)?;
        if write.write_id != write_id {
            return Err(Error::Protocol);
        }
        if acceptance == WriteAcceptance::Backpressure {
            return Ok(Submission::Backpressure);
        }
        let (sequence, write) = self.pending_writes.pop_front().unwrap();
        self.pending_bytes -= write.bytes.len();
        if acceptance == WriteAcceptance::Accepted {
            if self.accepted_output_bytes + write.bytes.len() > MAX_OUTPUT_QUEUE_BYTES {
                return Err(Error::Protocol);
            }
            self.accepted_output_bytes += write.bytes.len();
            self.accepted_output.push_back((sequence, write.bytes));
        }
        Ok(Submission::Accepted)
    }

    fn has_write_capacity(&self, bytes: usize) -> bool {
        self.pending_writes.len() < MAX_PENDING_WRITES
            && self.pending_bytes + self.accepted_output_bytes + bytes <= MAX_OUTPUT_QUEUE_BYTES
    }

    fn expect_sequence(&self, sequence: u64) -> Result<(), Error> {
        if sequence != self.next_sequence {
            return Err(Error::Protocol);
        }
        Ok(())
    }

    fn settle(&mut self) -> Result<Submission, Error> {
        let outcome = if self.pending_writes.is_empty() && self.unsettled_host_calls.is_empty() {
            ProgramOutcome::Completed {}
        } else {
            ProgramOutcome::UnsettledWork {}
        };
        self.outcome = Some(outcome);
        Ok(Submission::Accepted)
    }
}

fn generation(message: &ProgramHostMessage) -> u64 {
    match message {
        ProgramHostMessage::Launch { generation, .. }
        | ProgramHostMessage::Input { generation, .. }
        | ProgramHostMessage::Resize { generation, .. }
        | ProgramHostMessage::Close { generation, .. }
        | ProgramHostMessage::WriteResult { generation, .. }
        | ProgramHostMessage::Cancel { generation } => *generation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner() -> InProcessProgramRunner {
        InProcessProgramRunner::launch(
            developer_echo_program().unwrap(),
            7,
            TerminalSize {
                columns: 80,
                rows: 24,
            },
        )
        .unwrap()
    }

    #[test]
    fn program_manifest_and_wire_are_strict() {
        let program = developer_echo_program().unwrap();
        let mut snapshot = program.snapshot().clone();
        snapshot.manifest.version = 2;
        assert!(ApprovedProgram::new(snapshot).is_err());
        assert!(serde_json::from_str::<ProgramHostMessage>(
            r#"{"type":"cancel","generation":7,"extra":true}"#
        )
        .is_err());
        assert!(serde_json::from_str::<ProgramWorkerMessage>(
            r#"{"type":"finished","generation":7,"outcome":{"status":"completed","extra":1}}"#
        )
        .is_err());
        let mut manifest = serde_json::to_value(program.manifest()).unwrap();
        manifest["unknown"] = true.into();
        assert!(serde_json::from_value::<ProgramManifest>(manifest).is_err());
        let write = ProgramWorkerMessage::Write {
            generation: 7,
            write_id: 1,
            bytes: b"x".to_vec(),
        };
        write.validate().unwrap();
        let json = serde_json::to_string(&write).unwrap();
        assert_eq!(
            serde_json::from_str::<ProgramWorkerMessage>(&json).unwrap(),
            write
        );
    }

    #[test]
    fn bundle_and_wire_limits_are_bounded() {
        let mut source = developer_echo_program().unwrap().snapshot().clone();
        source.modules.insert(
            "main.js".into(),
            format!("/*{}*/ export const x=1;", "a".repeat(1_100_000)),
        );
        assert!(ApprovedProgram::new(source).is_err());
        let write = TerminalWrite {
            generation: 7,
            write_id: 1,
            bytes: vec![b'x'; MAX_OUTPUT_CHUNK_BYTES + 1],
        };
        assert!(write.validate().is_err());
        assert!(TerminalWrite {
            generation: 7,
            write_id: 1,
            bytes: vec![0xff],
        }
        .validate()
        .is_err());
        assert!(TerminalSize {
            columns: 1,
            rows: 24
        }
        .validate()
        .is_err());
    }

    #[test]
    fn snapshot_and_frame_decoders_accept_split_chunks() {
        let program = developer_echo_program().unwrap();
        let bytes = program.snapshot_bytes().unwrap();
        let mut loader = ProgramBundleLoader::begin(program.digest().into(), bytes.len()).unwrap();
        for chunk in bytes.chunks(31) {
            loader.push(chunk).unwrap();
        }
        assert_eq!(loader.remaining(), 0);
        assert_eq!(loader.finish().unwrap().digest(), program.digest());

        let message = ProgramHostMessage::Input {
            generation: 7,
            sequence: 1,
            bytes: b"split-\xff".to_vec(),
        };
        let frame = encode_program_frame(&message).unwrap();
        let mut decoder = ProgramWireDecoder::default();
        for byte in frame {
            decoder.feed(&[byte]).unwrap();
        }
        assert_eq!(decoder.pop_message(), Some(message));
        assert!(decoder.is_idle());
    }

    #[test]
    fn stale_generation_cannot_publish_or_advance_input() {
        let mut runner = runner();
        let stale = ProgramHostMessage::Input {
            generation: 6,
            sequence: 1,
            bytes: b"stale".to_vec(),
        };
        assert_eq!(runner.submit(&stale).unwrap(), Submission::StaleGeneration);
        assert!(runner.pending_write().is_none());
        assert_eq!(
            runner
                .submit(&ProgramHostMessage::Input {
                    generation: 7,
                    sequence: 1,
                    bytes: b"live".to_vec(),
                })
                .unwrap(),
            Submission::Accepted
        );
    }

    #[test]
    fn input_and_resize_require_one_ordered_sequence() {
        let mut runner = runner();
        assert!(runner
            .submit(&ProgramHostMessage::Input {
                generation: 7,
                sequence: 2,
                bytes: b"x".to_vec(),
            })
            .is_err());
        runner
            .submit(&ProgramHostMessage::Resize {
                generation: 7,
                sequence: 1,
                size: TerminalSize {
                    columns: 100,
                    rows: 40,
                },
            })
            .unwrap();
        assert_eq!(runner.size().columns, 100);
        assert_eq!(
            runner.next_event(),
            Some(ProgramEvent::ResizeApplied(TerminalSize {
                columns: 100,
                rows: 40,
            }))
        );
        assert!(runner
            .submit(&ProgramHostMessage::Resize {
                generation: 7,
                sequence: 3,
                size: TerminalSize {
                    columns: 100,
                    rows: 41,
                },
            })
            .is_err());
    }

    #[test]
    fn resize_cannot_overtake_an_unaccepted_earlier_write() {
        let mut runner = runner();
        runner
            .submit(&ProgramHostMessage::Input {
                generation: 7,
                sequence: 1,
                bytes: b"first".to_vec(),
            })
            .unwrap();
        runner
            .submit(&ProgramHostMessage::Resize {
                generation: 7,
                sequence: 2,
                size: TerminalSize {
                    columns: 81,
                    rows: 24,
                },
            })
            .unwrap();
        assert_eq!(runner.next_event(), None);
        runner.write_result(1, WriteAcceptance::Accepted).unwrap();
        assert_eq!(
            runner.next_event(),
            Some(ProgramEvent::Output(b"first".to_vec()))
        );
        assert_eq!(
            runner.next_event(),
            Some(ProgramEvent::ResizeApplied(TerminalSize {
                columns: 81,
                rows: 24,
            }))
        );
    }

    #[test]
    fn writes_apply_bounded_backpressure_without_dropping_bytes() {
        let mut runner = runner();
        let chunk = vec![b'x'; MAX_OUTPUT_CHUNK_BYTES];
        for sequence in 1..=4 {
            assert_eq!(
                runner
                    .submit(&ProgramHostMessage::Input {
                        generation: 7,
                        sequence,
                        bytes: chunk.clone(),
                    })
                    .unwrap(),
                Submission::Accepted
            );
        }
        assert_eq!(
            runner
                .submit(&ProgramHostMessage::Input {
                    generation: 7,
                    sequence: 5,
                    bytes: chunk.clone(),
                })
                .unwrap(),
            Submission::Backpressure
        );
        assert_eq!(runner.pending_write().unwrap().write_id, 1);
        assert_eq!(
            runner
                .write_result(1, WriteAcceptance::Backpressure)
                .unwrap(),
            Submission::Backpressure
        );
        assert_eq!(runner.pending_write().unwrap().write_id, 1);
        runner.write_result(1, WriteAcceptance::Accepted).unwrap();
        assert_eq!(
            runner.next_event(),
            Some(ProgramEvent::Output(vec![b'x'; MAX_OUTPUT_CHUNK_BYTES]))
        );
        assert_eq!(
            runner
                .submit(&ProgramHostMessage::Input {
                    generation: 7,
                    sequence: 5,
                    bytes: chunk,
                })
                .unwrap(),
            Submission::Accepted
        );
        assert_eq!(runner.pending_write().unwrap().write_id, 2);
    }

    #[test]
    fn normal_failed_and_unsettled_outcomes_are_distinct() {
        let mut normal = runner();
        assert_eq!(normal.finish(), ProgramOutcome::Completed {});
        let mut failed = runner();
        assert_eq!(
            failed.fail(ProgramFailure::ResourceLimit),
            ProgramOutcome::Failed {
                failure: ProgramFailure::ResourceLimit
            }
        );
        let mut unsettled = runner();
        unsettled.begin_host_call(1).unwrap();
        assert_eq!(unsettled.finish(), ProgramOutcome::UnsettledWork {});
        unsettled = runner();
        unsettled
            .submit(&ProgramHostMessage::Input {
                generation: 7,
                sequence: 1,
                bytes: b"x".to_vec(),
            })
            .unwrap();
        assert_eq!(unsettled.finish(), ProgramOutcome::UnsettledWork {});
    }

    #[test]
    fn cancel_is_explicit_and_discards_unaccepted_work() {
        let mut runner = runner();
        runner
            .submit(&ProgramHostMessage::Input {
                generation: 7,
                sequence: 1,
                bytes: b"x".to_vec(),
            })
            .unwrap();
        runner.begin_host_call(2).unwrap();
        runner
            .submit(&ProgramHostMessage::Cancel { generation: 7 })
            .unwrap();
        assert_eq!(runner.pending_write(), None);
        assert_eq!(runner.accepted_output_bytes(), 0);
        assert_eq!(
            runner.next_event(),
            Some(ProgramEvent::Finished(ProgramOutcome::Cancelled {}))
        );
    }
}
