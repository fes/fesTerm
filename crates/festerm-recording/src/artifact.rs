use std::{
    fmt,
    fs::File,
    io::{BufWriter, Read, Write},
    time::Duration,
};

use bincode::Options;
use festerm_core::{RecordingCheckpoint, Terminal, MAX_RECORDING_CHECKPOINT_BYTES};
use sha2::{Digest, Sha256};

use crate::{MAX_ARTIFACT_BYTES, MAX_DURATION, MAX_OUTPUT_BYTES, MAX_STREAMS};

const MAGIC: &[u8; 8] = b"FSTREC1\0";
const HEADER_BYTES: usize = 18;
const START: u8 = 1;
const OUTPUT: u8 = 2;
const CHECKPOINT: u8 = 3;
const RESIZE: u8 = 4;
const END: u8 = 5;
const FINISH: u8 = 6;

/// A recording-local session index, assigned in enrollment order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StreamId(u8);

impl StreamId {
    pub fn from_index(index: usize) -> Option<Self> {
        (1..=MAX_STREAMS)
            .contains(&index)
            .then(|| Self(u8::try_from(index).expect("stream limit fits in u8")))
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Why the shared recording stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinishReason {
    User,
    Lifecycle,
    ResourceLimit,
    IntegrityFailure,
    Interrupted,
}

impl FinishReason {
    pub const fn is_complete(self) -> bool {
        matches!(self, Self::User | Self::Lifecycle)
    }

    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::User => 1,
            Self::Lifecycle => 2,
            Self::ResourceLimit => 3,
            Self::IntegrityFailure => 4,
            Self::Interrupted => 5,
        }
    }

    pub(crate) const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::User),
            2 => Some(Self::Lifecycle),
            3 => Some(Self::ResourceLimit),
            4 => Some(Self::IntegrityFailure),
            5 => Some(Self::Interrupted),
            _ => None,
        }
    }
}

/// A validated artifact event. Restored terminals have no backend or queues.
pub enum ArtifactEvent {
    Started {
        at: Duration,
        stream: StreamId,
        terminal: Terminal,
    },
    Output {
        at: Duration,
        stream: StreamId,
        bytes: Vec<u8>,
    },
    Checkpoint {
        at: Duration,
        stream: StreamId,
        terminal: Terminal,
    },
    Resized {
        at: Duration,
        stream: StreamId,
        terminal: Terminal,
    },
    Ended {
        at: Duration,
        stream: StreamId,
    },
    Finished {
        at: Duration,
        reason: FinishReason,
    },
}

impl ArtifactEvent {
    pub const fn timestamp(&self) -> Duration {
        match self {
            Self::Started { at, .. }
            | Self::Output { at, .. }
            | Self::Checkpoint { at, .. }
            | Self::Resized { at, .. }
            | Self::Ended { at, .. }
            | Self::Finished { at, .. } => *at,
        }
    }
}

/// An invalid, truncated, or unreadable local recording.
#[derive(Debug)]
pub struct ArtifactError(String);

impl ArtifactError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ArtifactError {}

impl From<std::io::Error> for ArtifactError {
    fn from(error: std::io::Error) -> Self {
        Self::new(format!("recording I/O failed: {error}"))
    }
}

pub(crate) enum CapturedData {
    Start(RecordingCheckpoint),
    Output(Vec<u8>),
    Checkpoint(RecordingCheckpoint),
    Resize(RecordingCheckpoint),
    End,
}

pub(crate) struct CapturedEvent {
    pub(crate) at_ns: u64,
    pub(crate) stream: StreamId,
    pub(crate) data: CapturedData,
}

fn checkpoint_options() -> impl Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_RECORDING_CHECKPOINT_BYTES as u64)
        .reject_trailing_bytes()
}

pub(crate) struct ArtifactWriter {
    file: BufWriter<File>,
    bytes_written: u64,
}

impl ArtifactWriter {
    pub(crate) fn new(file: File) -> Result<Self, ArtifactError> {
        let mut file = BufWriter::new(file);
        file.write_all(MAGIC)?;
        Ok(Self {
            file,
            bytes_written: MAGIC.len() as u64,
        })
    }

    pub(crate) fn write_event(&mut self, event: &CapturedEvent) -> Result<(), ArtifactError> {
        match &event.data {
            CapturedData::Start(checkpoint) => self.write_frame(
                START,
                event.stream.0,
                event.at_ns,
                &encode_checkpoint(checkpoint)?,
            ),
            CapturedData::Output(bytes) => {
                if bytes.is_empty() || bytes.len() > MAX_OUTPUT_BYTES {
                    return Err(ArtifactError::new(
                        "recorded output exceeds its event limit",
                    ));
                }
                self.write_frame(OUTPUT, event.stream.0, event.at_ns, bytes)
            }
            CapturedData::Checkpoint(checkpoint) => self.write_frame(
                CHECKPOINT,
                event.stream.0,
                event.at_ns,
                &encode_checkpoint(checkpoint)?,
            ),
            CapturedData::Resize(checkpoint) => self.write_frame(
                RESIZE,
                event.stream.0,
                event.at_ns,
                &encode_checkpoint(checkpoint)?,
            ),
            CapturedData::End => self.write_frame(END, event.stream.0, event.at_ns, &[]),
        }
    }

    pub(crate) fn finish(mut self, at_ns: u64, reason: FinishReason) -> Result<(), ArtifactError> {
        self.write_frame(FINISH, 0, at_ns, &[reason.code()])?;
        self.file.flush()?;
        self.file.get_ref().sync_all()?;
        Ok(())
    }

    fn write_frame(
        &mut self,
        kind: u8,
        stream: u8,
        at_ns: u64,
        payload: &[u8],
    ) -> Result<(), ArtifactError> {
        let length = u32::try_from(payload.len())
            .map_err(|_| ArtifactError::new("recording frame length overflows"))?;
        let next = self
            .bytes_written
            .checked_add(HEADER_BYTES as u64)
            .and_then(|bytes| bytes.checked_add(u64::from(length)))
            .ok_or_else(|| ArtifactError::new("recording size overflows"))?;
        if next > MAX_ARTIFACT_BYTES {
            return Err(ArtifactError::new(
                "recording reached its source size limit",
            ));
        }
        let mut header = [0; HEADER_BYTES];
        header[0] = kind;
        header[1] = stream;
        header[2..10].copy_from_slice(&at_ns.to_le_bytes());
        header[10..14].copy_from_slice(&length.to_le_bytes());
        let mut checksum = crc32fast::Hasher::new();
        checksum.update(&header[..14]);
        checksum.update(payload);
        header[14..18].copy_from_slice(&checksum.finalize().to_le_bytes());
        self.file.write_all(&header)?;
        self.file.write_all(payload)?;
        self.bytes_written = next;
        Ok(())
    }
}

fn encode_checkpoint(checkpoint: &RecordingCheckpoint) -> Result<Vec<u8>, ArtifactError> {
    checkpoint
        .estimated_allocation_bytes()
        .map_err(|error| ArtifactError::new(format!("invalid recording checkpoint: {error}")))?;
    checkpoint_options()
        .serialize(checkpoint)
        .map_err(|error| ArtifactError::new(format!("checkpoint encoding failed: {error}")))
}

/// Streams a bounded, integrity-checked source without loading it into memory.
///
/// Incomplete sources can be inspected, but a caller must not treat one as a
/// completed export unless it reads a complete `Finished` event.
pub struct ArtifactReader<R> {
    input: R,
    bytes_read: u64,
    last_ns: u64,
    next_stream: usize,
    active: [bool; MAX_STREAMS],
    finished: bool,
    fingerprint: Sha256,
}

impl<R: Read> ArtifactReader<R> {
    pub fn new(mut input: R) -> Result<Self, ArtifactError> {
        let mut magic = [0; MAGIC.len()];
        input.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(ArtifactError::new("unsupported recording format"));
        }
        let mut fingerprint = Sha256::new();
        fingerprint.update(MAGIC);
        Ok(Self {
            input,
            bytes_read: MAGIC.len() as u64,
            last_ns: 0,
            next_stream: 1,
            active: [false; MAX_STREAMS],
            finished: false,
            fingerprint,
        })
    }

    /// Returns a fingerprint only after the complete source has been validated.
    pub fn fingerprint(&self) -> Option<[u8; 32]> {
        self.finished
            .then(|| self.fingerprint.clone().finalize().into())
    }

    pub fn next_event(&mut self) -> Result<Option<ArtifactEvent>, ArtifactError> {
        if self.finished {
            return Ok(None);
        }
        let mut header = [0; HEADER_BYTES];
        self.input
            .read_exact(&mut header)
            .map_err(|error| ArtifactError::new(format!("incomplete recording frame: {error}")))?;
        let kind = header[0];
        let stream_byte = header[1];
        let at_ns = u64::from_le_bytes(header[2..10].try_into().expect("fixed header"));
        let length = u32::from_le_bytes(header[10..14].try_into().expect("fixed header")) as usize;
        let expected_checksum =
            u32::from_le_bytes(header[14..18].try_into().expect("fixed header"));
        if at_ns < self.last_ns || at_ns > MAX_DURATION.as_nanos() as u64 {
            return Err(ArtifactError::new("recording timestamps are out of range"));
        }
        let maximum = match kind {
            START | CHECKPOINT | RESIZE => MAX_RECORDING_CHECKPOINT_BYTES,
            OUTPUT => MAX_OUTPUT_BYTES,
            END => 0,
            FINISH => 1,
            _ => return Err(ArtifactError::new("unknown recording frame type")),
        };
        if length > maximum
            || (matches!(kind, START | CHECKPOINT | RESIZE | OUTPUT | FINISH) && length == 0)
        {
            return Err(ArtifactError::new("recording frame exceeds its size limit"));
        }
        let next = self
            .bytes_read
            .checked_add(HEADER_BYTES as u64)
            .and_then(|bytes| bytes.checked_add(length as u64))
            .ok_or_else(|| ArtifactError::new("recording size overflows"))?;
        if next > MAX_ARTIFACT_BYTES {
            return Err(ArtifactError::new(
                "recording exceeds its source size limit",
            ));
        }
        let mut payload = vec![0; length];
        self.input.read_exact(&mut payload)?;
        let mut checksum = crc32fast::Hasher::new();
        checksum.update(&header[..14]);
        checksum.update(&payload);
        if checksum.finalize() != expected_checksum {
            return Err(ArtifactError::new(
                "recording frame checksum does not match",
            ));
        }
        self.fingerprint.update(header);
        self.fingerprint.update(&payload);
        self.bytes_read = next;
        self.last_ns = at_ns;
        let at = Duration::from_nanos(at_ns);

        if kind == FINISH {
            if stream_byte != 0 || self.next_stream == 1 || length != 1 {
                return Err(ArtifactError::new("invalid recording finish frame"));
            }
            let reason = FinishReason::from_code(payload[0])
                .ok_or_else(|| ArtifactError::new("unknown recording finish reason"))?;
            let mut trailing = [0; 1];
            if self.input.read(&mut trailing)? != 0 {
                return Err(ArtifactError::new("recording has trailing data"));
            }
            self.finished = true;
            return Ok(Some(ArtifactEvent::Finished { at, reason }));
        }

        let stream = StreamId::from_index(usize::from(stream_byte))
            .ok_or_else(|| ArtifactError::new("recording has an invalid stream index"))?;
        let slot = stream.index() - 1;
        if kind == START {
            if stream.index() != self.next_stream || self.active[slot] {
                return Err(ArtifactError::new(
                    "recording streams are not in enrollment order",
                ));
            }
            let terminal = decode_checkpoint(&payload)?;
            self.next_stream += 1;
            self.active[slot] = true;
            return Ok(Some(ArtifactEvent::Started {
                at,
                stream,
                terminal,
            }));
        }
        if !self.active[slot] {
            return Err(ArtifactError::new("recording event has no active stream"));
        }
        let event = match kind {
            OUTPUT => ArtifactEvent::Output {
                at,
                stream,
                bytes: payload,
            },
            CHECKPOINT => ArtifactEvent::Checkpoint {
                at,
                stream,
                terminal: decode_checkpoint(&payload)?,
            },
            RESIZE => ArtifactEvent::Resized {
                at,
                stream,
                terminal: decode_checkpoint(&payload)?,
            },
            END => {
                self.active[slot] = false;
                ArtifactEvent::Ended { at, stream }
            }
            _ => return Err(ArtifactError::new("unexpected recording frame")),
        };
        Ok(Some(event))
    }
}

fn decode_checkpoint(payload: &[u8]) -> Result<Terminal, ArtifactError> {
    let checkpoint: RecordingCheckpoint = checkpoint_options()
        .deserialize(payload)
        .map_err(|error| ArtifactError::new(format!("invalid recording checkpoint: {error}")))?;
    checkpoint
        .into_terminal()
        .map_err(|error| ArtifactError::new(format!("invalid recording checkpoint: {error}")))
}
