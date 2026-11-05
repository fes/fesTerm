//! Incremental framing and nonblocking I/O for the experimental Noise NN handshake.
//!
//! The initiator sends a two-byte big-endian length and its 32-byte Noise
//! message; the responder sends the same framing around its 48-byte reply.
//! Each role accepts only the exact expected length. No application data,
//! fingerprint comparison, peer identity, approval, or principal is exchanged.
//!
//! A successful handoff is only an [`UnverifiedChannel`]. Noise NN alone
//! authenticates neither endpoint, and the complete public fingerprint is not
//! a credential. Both endpoints must independently present their fingerprints
//! for trusted human comparison before the caller confirms either channel.
//! Never self-confirm using this endpoint's value or a value supplied over the
//! channel being verified. The caller owns human approval, deadlines,
//! cancellation, live privacy checks, and the transport; this module enables no
//! MCP endpoint.
//!
//! [`Interest`] describes work to attempt, not a new readiness edge. Try the
//! initial operation and every change of direction immediately. After
//! [`ReadProgress::Partial`], [`ReadProgress::Retry`], [`WriteProgress::Partial`]
//! or [`WriteProgress::Retry`], retry without waiting for a new edge, subject to
//! the caller's deadline and fairness budget. Only `Blocked` indicates an
//! actual `WouldBlock`. If a fairness budget is exhausted while progress is
//! possible, reschedule without relying on another edge.
//!
//! On cancellation, privacy loss, denial, EOF, or terminal I/O error, the caller
//! must also close the owned stream and discard any handed-off unverified
//! channel rather than retaining it for a later grant. Recheck the live
//! deadline and privacy boundary before handoff and confirmation. Never reuse
//! this state with another stream.

use std::{fmt, io, mem};

use super::{
    initiate, respond, ChannelError, ChannelPurpose, Initiator, UnverifiedChannel,
    FIRST_HANDSHAKE_BYTES, SECOND_HANDSHAKE_BYTES,
};

pub use crate::secure_record_io::WriteProgress;

const HEADER_BYTES: usize = 2;
const MAX_WIRE_BYTES: usize = HEADER_BYTES + SECOND_HANDSHAKE_BYTES;

#[derive(Debug)]
pub enum Error {
    Channel(ChannelError),
    InvalidFrame,
    Truncated,
    PeerClosed,
    /// Handoff was requested before the handshake and its writes completed.
    NotReady,
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Channel(error) => write!(formatter, "{error}"),
            Self::InvalidFrame => formatter.write_str("secure handshake frame refused"),
            Self::Truncated => formatter.write_str("secure handshake ended before completion"),
            Self::PeerClosed => formatter.write_str("secure peer closed before channel handoff"),
            Self::NotReady => formatter.write_str("secure handshake is not ready for handoff"),
            Self::Io(error) => write!(formatter, "secure handshake I/O failed: {:?}", error.kind()),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Channel(error) => Some(error),
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

/// The next operation to attempt; this is not itself a readiness notification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Interest {
    Read,
    Write,
    /// Only an unverified channel is available, not an authenticated peer.
    Complete,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadProgress {
    /// No read is currently requested; the supplied closure was not called.
    Idle,
    /// Wait for readable readiness before retrying.
    Blocked,
    /// Retry an interrupted read without waiting for a new readiness edge.
    Retry,
    /// Retry the remaining input without waiting for a new readiness edge.
    Partial,
    /// The responder must attempt its reply without waiting for a new edge.
    WriteReady,
    /// The handshake is complete; an unverified channel may be taken.
    Complete,
}

enum State {
    WriteFirst(Initiator),
    ReadReply(Initiator),
    ReadFirst,
    WriteReply(UnverifiedChannel),
    Ready(UnverifiedChannel),
    Closed,
}

struct Input {
    bytes: [u8; MAX_WIRE_BYTES],
    filled: usize,
    expected: usize,
}

impl Input {
    fn new(expected: usize) -> Self {
        Self {
            bytes: [0; MAX_WIRE_BYTES],
            filled: 0,
            expected,
        }
    }

    fn clear(&mut self) {
        self.bytes.fill(0);
        self.filled = 0;
    }
}

struct Output {
    bytes: [u8; MAX_WIRE_BYTES],
    len: usize,
    written: usize,
}

impl Output {
    fn empty() -> Self {
        Self {
            bytes: [0; MAX_WIRE_BYTES],
            len: 0,
            written: 0,
        }
    }

    fn frame(payload: &[u8]) -> Result<Self, Error> {
        if !matches!(
            payload.len(),
            FIRST_HANDSHAKE_BYTES | SECOND_HANDSHAKE_BYTES
        ) {
            return Err(Error::InvalidFrame);
        }
        let length = u16::try_from(payload.len()).map_err(|_| Error::InvalidFrame)?;
        let mut output = Self::empty();
        output.bytes[..HEADER_BYTES].copy_from_slice(&length.to_be_bytes());
        output.bytes[HEADER_BYTES..HEADER_BYTES + payload.len()].copy_from_slice(payload);
        output.len = HEADER_BYTES + payload.len();
        Ok(output)
    }
}

/// One handshake on one caller-owned stream, with fixed-size input and output.
///
/// The initiator cannot read its reply until the first frame has been written.
/// The responder retains its unverified channel until its entire reply is
/// written. Once taken, the channel cannot be taken again.
pub struct HandshakeIo {
    state: State,
    purpose: ChannelPurpose,
    input: Input,
    output: Output,
}

impl HandshakeIo {
    /// Create the adapter side and immediately queue its first handshake frame.
    pub fn initiator() -> Result<Self, Error> {
        Self::initiator_for(ChannelPurpose::Adapter)
    }

    /// Create a direct client handshake, separated from the stdio adapter.
    pub fn direct_initiator() -> Result<Self, Error> {
        Self::initiator_for(ChannelPurpose::Direct)
    }

    fn initiator_for(purpose: ChannelPurpose) -> Result<Self, Error> {
        let (initiator, first) = initiate(purpose).map_err(Error::Channel)?;
        Ok(Self {
            state: State::WriteFirst(initiator),
            purpose,
            input: Input::new(SECOND_HANDSHAKE_BYTES),
            output: Output::frame(&first)?,
        })
    }

    /// Create the application side, initially waiting for the first frame.
    pub fn responder() -> Self {
        Self::responder_for(ChannelPurpose::Adapter)
    }

    /// Create the application side of a direct client handshake.
    pub fn direct_responder() -> Self {
        Self::responder_for(ChannelPurpose::Direct)
    }

    fn responder_for(purpose: ChannelPurpose) -> Self {
        Self {
            state: State::ReadFirst,
            purpose,
            input: Input::new(FIRST_HANDSHAKE_BYTES),
            output: Output::empty(),
        }
    }

    pub fn interest(&self) -> Interest {
        match self.state {
            State::WriteFirst(_) | State::WriteReply(_) => Interest::Write,
            State::ReadFirst | State::ReadReply(_) => Interest::Read,
            State::Ready(_) => Interest::Complete,
            State::Closed => Interest::Closed,
        }
    }

    /// Discard all pending output and channel state. This is idempotent and
    /// does not close the caller-owned transport.
    pub fn close(&mut self) {
        self.state = State::Closed;
        self.input.clear();
        self.output = Output::empty();
    }

    fn fail<T>(&mut self, error: Error) -> Result<T, Error> {
        self.close();
        Err(error)
    }

    /// Report EOF even when the current interest is not reading. EOF before
    /// handoff always aborts, including after a complete handshake.
    pub fn end_input(&mut self) -> Result<(), Error> {
        let error = match self.state {
            State::Closed => return Err(Error::Channel(ChannelError::Closed)),
            State::Ready(_) => Error::PeerClosed,
            _ => Error::Truncated,
        };
        self.fail(error)
    }

    /// Perform one nonblocking read, requesting no more than the missing
    /// header or payload. Any following stream bytes remain with the caller.
    /// The closure must follow the `Read` contract and report only bytes
    /// actually consumed from this stream. A zero-length read is EOF; all
    /// returned errors are terminal.
    pub fn read_pending(
        &mut self,
        read: impl FnOnce(&mut [u8]) -> io::Result<usize>,
    ) -> Result<ReadProgress, Error> {
        // An unwinding I/O callback may have consumed bytes; never resume it.
        let state = match mem::replace(&mut self.state, State::Closed) {
            state @ (State::ReadFirst | State::ReadReply(_)) => state,
            State::Closed => return Err(Error::Channel(ChannelError::Closed)),
            state => {
                self.state = state;
                return Ok(ReadProgress::Idle);
            }
        };
        let end = if self.input.filled < HEADER_BYTES {
            HEADER_BYTES
        } else {
            HEADER_BYTES + self.input.expected
        };
        let available = end - self.input.filled;
        let result = read(&mut self.input.bytes[self.input.filled..end]);
        match result {
            Ok(0) => self.fail(Error::Truncated),
            Ok(count) if count > available => self.fail(Error::InvalidFrame),
            Ok(count) => {
                self.input.filled += count;
                if self.input.filled == HEADER_BYTES
                    && usize::from(u16::from_be_bytes([
                        self.input.bytes[0],
                        self.input.bytes[1],
                    ])) != self.input.expected
                {
                    return self.fail(Error::InvalidFrame);
                }
                if self.input.filled < HEADER_BYTES + self.input.expected {
                    self.state = state;
                    return Ok(ReadProgress::Partial);
                }
                let payload = &self.input.bytes[HEADER_BYTES..HEADER_BYTES + self.input.expected];
                match state {
                    State::ReadFirst => {
                        let (channel, reply) = match respond(payload, self.purpose) {
                            Ok(pair) => pair,
                            Err(error) => return self.fail(Error::Channel(error)),
                        };
                        self.output = match Output::frame(&reply) {
                            Ok(output) => output,
                            Err(error) => return self.fail(error),
                        };
                        self.input.clear();
                        self.state = State::WriteReply(channel);
                        Ok(ReadProgress::WriteReady)
                    }
                    State::ReadReply(initiator) => {
                        let channel = match initiator.finish(payload) {
                            Ok(channel) => channel,
                            Err(error) => return self.fail(Error::Channel(error)),
                        };
                        self.input.clear();
                        self.state = State::Ready(channel);
                        Ok(ReadProgress::Complete)
                    }
                    _ => unreachable!("only reading states reach frame completion"),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                self.state = state;
                Ok(ReadProgress::Blocked)
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                self.state = state;
                Ok(ReadProgress::Retry)
            }
            Err(error) => self.fail(Error::Io(error)),
        }
    }

    /// Perform one physical nonblocking write of the queued handshake frame.
    /// The closure must write directly to the owned stream and report exactly
    /// the bytes accepted; a buffered queue or fabricated count does not meet
    /// this contract. Acceptance by the OS does not prove peer receipt. The
    /// caller must hold any required live privacy/admission guard through the
    /// physical write and close on denial. `Idle` never calls the closure.
    /// `Complete` finishes this output frame, not necessarily the entire
    /// handshake; inspect [`Self::interest`] and attempt a newly requested
    /// operation immediately. All returned errors are terminal.
    pub fn write_pending(
        &mut self,
        write: impl FnOnce(&[u8]) -> io::Result<usize>,
    ) -> Result<WriteProgress, Error> {
        // An unwinding I/O callback may have written bytes; never resume it.
        let state = match mem::replace(&mut self.state, State::Closed) {
            state @ (State::WriteFirst(_) | State::WriteReply(_)) => state,
            State::Closed => return Err(Error::Channel(ChannelError::Closed)),
            state => {
                self.state = state;
                return Ok(WriteProgress::Idle);
            }
        };
        let pending = &self.output.bytes[self.output.written..self.output.len];
        if pending.is_empty() {
            return self.fail(Error::InvalidFrame);
        }
        match write(pending) {
            Ok(0) => self.fail(Error::Io(io::ErrorKind::WriteZero.into())),
            Ok(count) if count > pending.len() => self.fail(Error::InvalidFrame),
            Ok(count) => {
                self.output.written += count;
                if self.output.written < self.output.len {
                    self.state = state;
                    return Ok(WriteProgress::Partial);
                }
                self.output = Output::empty();
                self.state = match state {
                    State::WriteFirst(initiator) => State::ReadReply(initiator),
                    State::WriteReply(channel) => State::Ready(channel),
                    _ => unreachable!("only writing states have pending output"),
                };
                Ok(WriteProgress::Complete)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                self.state = state;
                Ok(WriteProgress::Blocked)
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                self.state = state;
                Ok(WriteProgress::Retry)
            }
            Err(error) => self.fail(Error::Io(error)),
        }
    }

    /// Take the channel only after every required outbound byte was accepted
    /// by the physical writer and the inbound handshake frame was validated.
    /// `NotReady` is nonterminal; all successful handoffs are single-use.
    pub fn take_unverified(&mut self) -> Result<UnverifiedChannel, Error> {
        match mem::replace(&mut self.state, State::Closed) {
            State::Ready(channel) => {
                self.input.clear();
                self.output = Output::empty();
                Ok(channel)
            }
            State::Closed => Err(Error::Channel(ChannelError::Closed)),
            state => {
                self.state = state;
                Err(Error::NotReady)
            }
        }
    }
}
