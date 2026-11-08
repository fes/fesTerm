use std::{
    fmt,
    fs::File,
    io::{self, BufWriter, Cursor, Read, Take, Write},
    time::Duration,
};

use bincode::Options;
use festerm_core::{
    ColorScheme, Dimensions, RecordingCheckpoint, Terminal, TerminalSemantics,
    MAX_RECORDING_CHECKPOINT_BYTES, MAX_RECORDING_SCROLLBACK_BYTES,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{MAX_ARTIFACT_BYTES, MAX_DURATION, MAX_OUTPUT_BYTES, MAX_OUTPUT_REPEATS, MAX_STREAMS};

const LEGACY_MAGIC: &[u8; 8] = b"FSTREC2\0";
const MAGIC: &[u8; 8] = b"FSTREC3\0";
const HEADER_BYTES: usize = 18;
const START: u8 = 1;
const OUTPUT: u8 = 2;
const RESIZE: u8 = 4;
const END: u8 = 5;
const FINISH: u8 = 6;
const RESET: u8 = 7;
const CONFIGURE: u8 = 8;
const MAX_CONFIG_BYTES: usize = 256;
const NO_REPEAT_CHARACTER: u32 = 0x11_0000;

/// Byte offset of a REP's final byte and its privacy-projected character.
pub type RepeatHint = (usize, Option<char>);

#[derive(Deserialize, Serialize)]
struct RecordedConfiguration {
    color_scheme: ColorScheme,
    scrollback_limit_bytes: usize,
    history_capped: bool,
}

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
    /// A separately confirmed export of an incomplete source's valid prefix.
    RecoveredPrefix,
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
            Self::RecoveredPrefix => 6,
        }
    }

    pub(crate) const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::User),
            2 => Some(Self::Lifecycle),
            3 => Some(Self::ResourceLimit),
            4 => Some(Self::IntegrityFailure),
            5 => Some(Self::Interrupted),
            6 => Some(Self::RecoveredPrefix),
            _ => None,
        }
    }
}

/// A validated artifact event. Restored terminals have no backend or queues.
pub enum ArtifactEvent {
    Started {
        at: Duration,
        stream: StreamId,
        terminal: Box<Terminal>,
        history_capped: bool,
    },
    Output {
        at: Duration,
        stream: StreamId,
        bytes: Vec<u8>,
        repeats: Vec<RepeatHint>,
    },
    Resized {
        at: Duration,
        stream: StreamId,
        dimensions: Dimensions,
    },
    Reset {
        at: Duration,
        stream: StreamId,
    },
    Configured {
        at: Duration,
        stream: StreamId,
        color_scheme: ColorScheme,
        scrollback_limit_bytes: usize,
        history_capped: bool,
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
            | Self::Resized { at, .. }
            | Self::Reset { at, .. }
            | Self::Configured { at, .. }
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

/// A checked prefix of an interrupted recording. It is not a complete source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveryPrefix {
    length: u64,
    duration: Duration,
    stream_count: usize,
    fingerprint: [u8; 32],
    finish: [u8; HEADER_BYTES + 1],
}

impl RecoveryPrefix {
    pub const fn length(self) -> u64 {
        self.length
    }

    pub const fn duration(self) -> Duration {
        self.duration
    }

    pub const fn stream_count(self) -> usize {
        self.stream_count
    }

    /// Fingerprint of the exact prefix followed by its synthetic recovery end.
    pub const fn fingerprint(self) -> [u8; 32] {
        self.fingerprint
    }

    /// Reads only the checked prefix and appends an explicitly recovered end.
    pub fn reader<R: Read>(self, input: R) -> RecoveryReader<R> {
        RecoveryReader {
            prefix: input.take(self.length),
            finish: Cursor::new(self.finish),
        }
    }
}

pub struct RecoveryReader<R> {
    prefix: Take<R>,
    finish: Cursor<[u8; HEADER_BYTES + 1]>,
}

impl<R: Read> Read for RecoveryReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.prefix.limit() != 0 {
            let count = self.prefix.read(buffer)?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the inspected recording prefix was truncated",
                ));
            }
            return Ok(count);
        }
        self.finish.read(buffer)
    }
}

pub(crate) enum CapturedData {
    Start(Box<RecordingCheckpoint>),
    Output {
        bytes: Vec<u8>,
        repeats: Vec<RepeatHint>,
    },
    Resize(Dimensions),
    Reset,
    Configure {
        color_scheme: ColorScheme,
        scrollback_limit_bytes: usize,
        history_capped: bool,
    },
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
            CapturedData::Output { bytes, repeats } => self.write_frame(
                OUTPUT,
                event.stream.0,
                event.at_ns,
                &encode_output(bytes, repeats)?,
            ),
            CapturedData::Resize(dimensions) => {
                Terminal::validate_recording_dimensions(*dimensions)
                    .map_err(|error| ArtifactError::new(error.to_string()))?;
                let columns = u32::try_from(dimensions.columns())
                    .map_err(|_| ArtifactError::new("recording columns overflow"))?;
                let rows = u32::try_from(dimensions.rows())
                    .map_err(|_| ArtifactError::new("recording rows overflow"))?;
                let mut payload = [0; 8];
                payload[..4].copy_from_slice(&columns.to_le_bytes());
                payload[4..].copy_from_slice(&rows.to_le_bytes());
                self.write_frame(RESIZE, event.stream.0, event.at_ns, &payload)
            }
            CapturedData::Reset => self.write_frame(RESET, event.stream.0, event.at_ns, &[]),
            CapturedData::Configure {
                color_scheme,
                scrollback_limit_bytes,
                history_capped,
            } => {
                if *scrollback_limit_bytes > MAX_RECORDING_SCROLLBACK_BYTES {
                    return Err(ArtifactError::new(
                        "recording history limit exceeds its bound",
                    ));
                }
                let configuration = RecordedConfiguration {
                    color_scheme: *color_scheme,
                    scrollback_limit_bytes: *scrollback_limit_bytes,
                    history_capped: *history_capped,
                };
                let payload =
                    configuration_options()
                        .serialize(&configuration)
                        .map_err(|error| {
                            ArtifactError::new(format!(
                                "recording configuration encoding failed: {error}"
                            ))
                        })?;
                self.write_frame(CONFIGURE, event.stream.0, event.at_ns, &payload)
            }
            CapturedData::End => self.write_frame(END, event.stream.0, event.at_ns, &[]),
        }
    }

    pub(crate) fn finish(mut self, at_ns: u64, reason: FinishReason) -> Result<(), ArtifactError> {
        self.write_frame(FINISH, 0, at_ns, &[reason.code()])?;
        self.file.flush()?;
        self.file.get_ref().sync_all()?;
        Ok(())
    }

    pub(crate) fn flush(&mut self) -> Result<(), ArtifactError> {
        self.file.flush()?;
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
    let mut payload = Vec::new();
    payload.push(match checkpoint.semantics() {
        TerminalSemantics::Legacy => 0,
        TerminalSemantics::Current => 1,
    });
    checkpoint_options()
        .serialize_into(&mut payload, checkpoint)
        .map_err(|error| ArtifactError::new(format!("checkpoint encoding failed: {error}")))?;
    Ok(payload)
}

fn configuration_options() -> impl Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_CONFIG_BYTES as u64)
        .reject_trailing_bytes()
}

fn encode_output(bytes: &[u8], repeats: &[RepeatHint]) -> Result<Vec<u8>, ArtifactError> {
    if bytes.is_empty() || bytes.len() > MAX_OUTPUT_BYTES || repeats.len() > MAX_OUTPUT_REPEATS {
        return Err(ArtifactError::new(
            "recorded output exceeds its event limit",
        ));
    }
    let mut payload = Vec::with_capacity(4 + repeats.len() * 8 + bytes.len());
    payload.extend_from_slice(&(repeats.len() as u32).to_le_bytes());
    let mut previous = None;
    for &(offset, character) in repeats {
        if offset >= bytes.len()
            || bytes[offset] != b'b'
            || previous.is_some_and(|last| offset <= last)
        {
            return Err(ArtifactError::new("recording repeat hints are invalid"));
        }
        payload.extend_from_slice(&(offset as u32).to_le_bytes());
        payload.extend_from_slice(
            &character
                .map_or(NO_REPEAT_CHARACTER, u32::from)
                .to_le_bytes(),
        );
        previous = Some(offset);
    }
    payload.extend_from_slice(bytes);
    Ok(payload)
}

fn decode_output(mut payload: Vec<u8>) -> Result<(Vec<u8>, Vec<RepeatHint>), ArtifactError> {
    if payload.len() < 5 {
        return Err(ArtifactError::new("recorded output is truncated"));
    }
    let count = u32::from_le_bytes(payload[..4].try_into().expect("fixed count")) as usize;
    if count > MAX_OUTPUT_REPEATS {
        return Err(ArtifactError::new("recording has too many repeat hints"));
    }
    let start = 4 + count * 8;
    if start >= payload.len() || payload.len() - start > MAX_OUTPUT_BYTES {
        return Err(ArtifactError::new(
            "recorded output exceeds its event limit",
        ));
    }
    let mut repeats = Vec::with_capacity(count);
    for entry in payload[4..start].as_chunks::<8>().0 {
        let offset = u32::from_le_bytes(entry[..4].try_into().expect("fixed offset")) as usize;
        let code = u32::from_le_bytes(entry[4..].try_into().expect("fixed character"));
        let character = if code == NO_REPEAT_CHARACTER {
            None
        } else {
            Some(
                char::from_u32(code)
                    .ok_or_else(|| ArtifactError::new("recording repeat character is invalid"))?,
            )
        };
        if offset >= payload.len() - start
            || payload[start + offset] != b'b'
            || repeats
                .last()
                .is_some_and(|(previous, _)| offset <= *previous)
        {
            return Err(ArtifactError::new("recording repeat hints are invalid"));
        }
        repeats.push((offset, character));
    }
    Ok((payload.split_off(start), repeats))
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
    legacy: bool,
}

impl<R: Read> ArtifactReader<R> {
    pub fn new(mut input: R) -> Result<Self, ArtifactError> {
        let mut magic = [0; MAGIC.len()];
        input.read_exact(&mut magic)?;
        if &magic == b"FSTREC1\0" {
            return Err(ArtifactError::new(
                "legacy recording format cannot be safely exported",
            ));
        }
        let legacy = &magic == LEGACY_MAGIC;
        if !legacy && &magic != MAGIC {
            return Err(ArtifactError::new("unsupported recording format"));
        }
        let mut fingerprint = Sha256::new();
        fingerprint.update(magic);
        Ok(Self {
            input,
            bytes_read: MAGIC.len() as u64,
            last_ns: 0,
            next_stream: 1,
            active: [false; MAX_STREAMS],
            finished: false,
            fingerprint,
            legacy,
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
            START => MAX_RECORDING_CHECKPOINT_BYTES + usize::from(!self.legacy),
            OUTPUT => MAX_OUTPUT_BYTES + 4 + MAX_OUTPUT_REPEATS * 8,
            RESIZE => 8,
            END | RESET => 0,
            CONFIGURE => MAX_CONFIG_BYTES,
            FINISH => 1,
            _ => return Err(ArtifactError::new("unknown recording frame type")),
        };
        if length > maximum
            || (matches!(kind, START | RESIZE | OUTPUT | CONFIGURE | FINISH) && length == 0)
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
        let at = Duration::from_nanos(at_ns);
        let mut fingerprint = self.fingerprint.clone();
        fingerprint.update(header);
        fingerprint.update(&payload);

        let event = if kind == FINISH {
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
            ArtifactEvent::Finished { at, reason }
        } else {
            let stream = StreamId::from_index(usize::from(stream_byte))
                .ok_or_else(|| ArtifactError::new("recording has an invalid stream index"))?;
            let slot = stream.index() - 1;
            if kind == START {
                if stream.index() != self.next_stream || self.active[slot] {
                    return Err(ArtifactError::new(
                        "recording streams are not in enrollment order",
                    ));
                }
                let (terminal, history_capped) = decode_checkpoint(&payload, self.legacy)?;
                self.next_stream += 1;
                self.active[slot] = true;
                ArtifactEvent::Started {
                    at,
                    stream,
                    terminal: Box::new(terminal),
                    history_capped,
                }
            } else {
                if !self.active[slot] {
                    return Err(ArtifactError::new("recording event has no active stream"));
                }
                match kind {
                    OUTPUT => {
                        let (bytes, repeats) = decode_output(payload)?;
                        ArtifactEvent::Output {
                            at,
                            stream,
                            bytes,
                            repeats,
                        }
                    }
                    RESIZE => {
                        if payload.len() != 8 {
                            return Err(ArtifactError::new("recording resize is invalid"));
                        }
                        let columns =
                            u32::from_le_bytes(payload[..4].try_into().expect("fixed columns"));
                        let rows = u32::from_le_bytes(payload[4..].try_into().expect("fixed rows"));
                        let dimensions = Dimensions::new(columns as usize, rows as usize)
                            .map_err(|error| ArtifactError::new(error.to_string()))?;
                        Terminal::validate_recording_dimensions(dimensions)
                            .map_err(|error| ArtifactError::new(error.to_string()))?;
                        ArtifactEvent::Resized {
                            at,
                            stream,
                            dimensions,
                        }
                    }
                    RESET => ArtifactEvent::Reset { at, stream },
                    CONFIGURE => {
                        let configuration: RecordedConfiguration = configuration_options()
                            .deserialize(&payload)
                            .map_err(|error| {
                                ArtifactError::new(format!(
                                    "invalid recording configuration: {error}"
                                ))
                            })?;
                        if configuration.scrollback_limit_bytes > MAX_RECORDING_SCROLLBACK_BYTES {
                            return Err(ArtifactError::new(
                                "recording history limit exceeds its bound",
                            ));
                        }
                        ArtifactEvent::Configured {
                            at,
                            stream,
                            color_scheme: configuration.color_scheme,
                            scrollback_limit_bytes: configuration.scrollback_limit_bytes,
                            history_capped: configuration.history_capped,
                        }
                    }
                    END => {
                        self.active[slot] = false;
                        ArtifactEvent::Ended { at, stream }
                    }
                    _ => return Err(ArtifactError::new("unexpected recording frame")),
                }
            }
        };
        self.fingerprint = fingerprint;
        self.bytes_read = next;
        self.last_ns = at_ns;
        Ok(Some(event))
    }

    pub(crate) fn recovery_prefix(&self) -> Option<RecoveryPrefix> {
        if self.finished || self.next_stream == 1 {
            return None;
        }
        let mut finish = [0; HEADER_BYTES + 1];
        finish[0] = FINISH;
        finish[2..10].copy_from_slice(&self.last_ns.to_le_bytes());
        finish[10..14].copy_from_slice(&1u32.to_le_bytes());
        finish[HEADER_BYTES] = FinishReason::RecoveredPrefix.code();
        let mut checksum = crc32fast::Hasher::new();
        checksum.update(&finish[..14]);
        checksum.update(&finish[HEADER_BYTES..]);
        finish[14..18].copy_from_slice(&checksum.finalize().to_le_bytes());
        let mut fingerprint = self.fingerprint.clone();
        fingerprint.update(finish);
        Some(RecoveryPrefix {
            length: self.bytes_read,
            duration: Duration::from_nanos(self.last_ns),
            stream_count: self.next_stream - 1,
            fingerprint: fingerprint.finalize().into(),
            finish,
        })
    }
}

fn decode_checkpoint(payload: &[u8], legacy: bool) -> Result<(Terminal, bool), ArtifactError> {
    let (semantics, payload) = if legacy {
        (TerminalSemantics::Legacy, payload)
    } else {
        match payload.split_first() {
            Some((0, payload)) => (TerminalSemantics::Legacy, payload),
            Some((1, payload)) => (TerminalSemantics::Current, payload),
            _ => {
                return Err(ArtifactError::new(
                    "unsupported recording terminal semantics",
                ))
            }
        }
    };
    let checkpoint: RecordingCheckpoint = checkpoint_options()
        .deserialize(payload)
        .map_err(|error| ArtifactError::new(format!("invalid recording checkpoint: {error}")))?;
    let history_capped = checkpoint.history_capped();
    let mut terminal = checkpoint
        .into_terminal()
        .map_err(|error| ArtifactError::new(format!("invalid recording checkpoint: {error}")))?;
    terminal.set_semantics(semantics);
    Ok((terminal, history_capped))
}
