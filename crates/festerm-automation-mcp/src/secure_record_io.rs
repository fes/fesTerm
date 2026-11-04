//! Bounded incremental framing for an independently verified Noise channel.
//!
//! This is not a principal, grant, transport, or privacy monitor. Callers must
//! enforce handshake/frame deadlines and revalidate the exact disclosure
//! boundary around every physical write. The writer supplied here must be
//! nonblocking. A caller must close the channel on cancellation or denied
//! authorization rather than retaining output for a later grant.

use std::{fmt, io};

use crate::secure_channel::{Channel, ChannelError, MAX_RECORD_BYTES, MIN_RECORD_BYTES};

const HEADER_BYTES: usize = 2;

#[derive(Debug)]
pub enum Error {
    Channel(ChannelError),
    InvalidRecord,
    Truncated,
    Busy,
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Channel(error) => write!(formatter, "{error}"),
            Self::InvalidRecord => formatter.write_str("secure record refused"),
            Self::Truncated => formatter.write_str("secure stream ended with an incomplete frame"),
            Self::Busy => formatter.write_str("secure frame output is still pending"),
            Self::Io(error) => write!(formatter, "secure record I/O failed: {error}"),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteProgress {
    Idle,
    /// Wait for writable readiness before retrying.
    Blocked,
    /// Retry without waiting for a new readiness edge, subject to the deadline.
    Retry,
    /// Retry the remaining bytes without waiting for a new readiness edge.
    Partial,
    Complete,
}

struct Output {
    bytes: Vec<u8>,
    written: usize,
}

/// One verified channel with at most one pending frame in each direction.
pub struct RecordIo {
    channel: Channel,
    header: [u8; HEADER_BYTES],
    header_len: usize,
    record_len: Option<usize>,
    record: Vec<u8>,
    awaiting_record: bool,
    output: Option<Output>,
    closed: bool,
}

impl RecordIo {
    /// Take a newly verified channel before it has sent or received records.
    pub fn new(mut channel: Channel) -> Result<Self, Error> {
        if let Err(error) = channel.require_fresh() {
            channel.close();
            return Err(Error::Channel(error));
        }
        Ok(Self {
            channel,
            header: [0; HEADER_BYTES],
            header_len: 0,
            record_len: None,
            record: Vec::new(),
            awaiting_record: false,
            output: None,
            closed: false,
        })
    }
}

impl RecordIo {
    fn require_open(&self) -> Result<(), Error> {
        if self.closed {
            Err(Error::Channel(ChannelError::Closed))
        } else {
            Ok(())
        }
    }

    pub fn close(&mut self) {
        self.closed = true;
        self.channel.close();
        self.output = None;
        self.record.clear();
    }

    /// Encrypt one frame. A second frame cannot be queued until the first is
    /// completely written, so a failed partial write cannot reuse Noise state.
    pub fn prepare_send(&mut self, frame: &[u8]) -> Result<(), Error> {
        self.require_open()?;
        if self.output.is_some() {
            return Err(Error::Busy);
        }
        let records = match self.channel.seal_frame(frame) {
            Ok(records) => records,
            Err(error) => {
                self.close();
                return Err(Error::Channel(error));
            }
        };
        let mut bytes = Vec::with_capacity(
            records
                .iter()
                .map(|record| HEADER_BYTES + record.len())
                .sum(),
        );
        for record in records {
            let size = u16::try_from(record.len()).map_err(|_| {
                self.close();
                Error::InvalidRecord
            })?;
            bytes.extend_from_slice(&size.to_be_bytes());
            bytes.extend_from_slice(&record);
        }
        self.output = Some(Output { bytes, written: 0 });
        Ok(())
    }

    /// Perform one physical nonblocking write. The caller must hold its live
    /// disclosure/admission guard for the entire supplied write closure and
    /// close the channel if that guard cannot be acquired.
    pub fn write_pending(
        &mut self,
        write: impl FnOnce(&[u8]) -> io::Result<usize>,
    ) -> Result<WriteProgress, Error> {
        self.require_open()?;
        let Some(output) = self.output.as_mut() else {
            return Ok(WriteProgress::Idle);
        };
        let pending = &output.bytes[output.written..];
        match write(pending) {
            Ok(0) => {
                self.close();
                Err(Error::Io(io::ErrorKind::WriteZero.into()))
            }
            Ok(written) if written > pending.len() => {
                self.close();
                Err(Error::InvalidRecord)
            }
            Ok(written) => {
                output.written += written;
                if output.written == output.bytes.len() {
                    self.output = None;
                    Ok(WriteProgress::Complete)
                } else {
                    Ok(WriteProgress::Partial)
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(WriteProgress::Blocked),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(WriteProgress::Retry),
            Err(error) => {
                self.close();
                Err(Error::Io(error))
            }
        }
    }

    /// Consume at most one plaintext frame, reporting the exact input prefix
    /// used. Ciphertext is authenticated before any plaintext is returned.
    pub fn receive(&mut self, input: &[u8]) -> Result<(usize, Option<Vec<u8>>), Error> {
        self.require_open()?;
        let mut used = 0;
        while used < input.len() {
            if self.header_len < HEADER_BYTES {
                let count = (HEADER_BYTES - self.header_len).min(input.len() - used);
                self.header[self.header_len..self.header_len + count]
                    .copy_from_slice(&input[used..used + count]);
                self.header_len += count;
                used += count;
                if self.header_len < HEADER_BYTES {
                    break;
                }
                let length = usize::from(u16::from_be_bytes(self.header));
                if !(MIN_RECORD_BYTES..=MAX_RECORD_BYTES).contains(&length) {
                    self.close();
                    return Err(Error::InvalidRecord);
                }
                self.record_len = Some(length);
                self.record.clear();
            }

            let length = self.record_len.expect("complete header has a length");
            let count = (length - self.record.len()).min(input.len() - used);
            self.record.extend_from_slice(&input[used..used + count]);
            used += count;
            if self.record.len() != length {
                break;
            }
            let frame = match self.channel.open_record(&self.record) {
                Ok(frame) => frame,
                Err(error) => {
                    self.close();
                    return Err(Error::Channel(error));
                }
            };
            self.header_len = 0;
            self.record_len = None;
            self.record.clear();
            self.awaiting_record = frame.is_none();
            if frame.is_some() {
                return Ok((used, frame));
            }
        }
        Ok((used, None))
    }

    /// EOF always closes the channel; incomplete input or output is an
    /// explicit truncation rather than a complete application message.
    pub fn end_input(&mut self) -> Result<(), Error> {
        self.require_open()?;
        let truncated = self.header_len != 0 || self.awaiting_record || self.output.is_some();
        self.close();
        if truncated {
            Err(Error::Truncated)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        secure_handshake_io::{HandshakeIo, ReadProgress},
        MAX_MESSAGE_BYTES,
    };
    use std::io::Read;

    fn read_handshake(handshake: &mut HandshakeIo, mut wire: &[u8]) {
        loop {
            match handshake.read_pending(|buffer| wire.read(buffer)).unwrap() {
                ReadProgress::Partial => {}
                ReadProgress::WriteReady | ReadProgress::Complete => break,
                progress => panic!("unexpected handshake progress: {progress:?}"),
            }
        }
        assert!(wire.is_empty());
    }

    fn channels() -> (Channel, Channel) {
        let mut adapter = HandshakeIo::initiator().unwrap();
        let mut application = HandshakeIo::responder();
        let mut first = Vec::new();
        assert_eq!(
            adapter
                .write_pending(|pending| {
                    first.extend_from_slice(pending);
                    Ok(pending.len())
                })
                .unwrap(),
            WriteProgress::Complete
        );
        read_handshake(&mut application, &first);
        let mut reply = Vec::new();
        assert_eq!(
            application
                .write_pending(|pending| {
                    reply.extend_from_slice(pending);
                    Ok(pending.len())
                })
                .unwrap(),
            WriteProgress::Complete
        );
        read_handshake(&mut adapter, &reply);
        let adapter = adapter.take_unverified().unwrap();
        let application = application.take_unverified().unwrap();
        let application_fingerprint = application.fingerprint().to_string();
        let adapter_fingerprint = adapter.fingerprint().to_string();
        (
            adapter.confirm(&application_fingerprint).unwrap(),
            application.confirm(&adapter_fingerprint).unwrap(),
        )
    }

    fn pair() -> (RecordIo, RecordIo) {
        let (left, right) = channels();
        (RecordIo::new(left).unwrap(), RecordIo::new(right).unwrap())
    }

    fn drain(sender: &mut RecordIo, chunk_size: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let progress = sender
                .write_pending(|pending| {
                    let count = pending.len().min(chunk_size);
                    bytes.extend_from_slice(&pending[..count]);
                    Ok(count)
                })
                .unwrap();
            if matches!(progress, WriteProgress::Complete | WriteProgress::Idle) {
                return bytes;
            }
        }
    }

    #[test]
    fn fragmented_records_return_one_complete_authenticated_frame() {
        let (mut sender, mut receiver) = pair();
        let expected = vec![42; MAX_MESSAGE_BYTES];
        sender.prepare_send(&expected).unwrap();
        let wire = drain(&mut sender, 17);
        let mut received = None;
        for chunk in wire.chunks(19) {
            let (used, frame) = receiver.receive(chunk).unwrap();
            assert_eq!(used, chunk.len());
            if frame.is_some() {
                assert!(received.is_none());
                received = frame;
            }
        }
        assert_eq!(received, Some(expected));
        receiver.end_input().unwrap();
    }

    #[test]
    fn full_duplex_can_read_while_output_is_blocked_or_partial() {
        let (mut left, mut right) = pair();
        left.prepare_send(b"left").unwrap();
        right.prepare_send(b"right").unwrap();
        assert!(matches!(left.prepare_send(b"again"), Err(Error::Busy)));
        assert_eq!(
            left.write_pending(|_| Err(io::ErrorKind::WouldBlock.into()))
                .unwrap(),
            WriteProgress::Blocked
        );
        assert_eq!(
            left.write_pending(|_| Err(io::ErrorKind::Interrupted.into()))
                .unwrap(),
            WriteProgress::Retry
        );
        let mut prefix = Vec::new();
        assert_eq!(
            left.write_pending(|pending| {
                prefix.extend_from_slice(&pending[..1]);
                Ok(1)
            })
            .unwrap(),
            WriteProgress::Partial
        );
        let right_wire = drain(&mut right, 2);
        let (used, frame) = left.receive(&right_wire).unwrap();
        assert_eq!(used, right_wire.len());
        assert_eq!(frame.as_deref(), Some(&b"right"[..]));
        prefix.extend_from_slice(&drain(&mut left, 3));
        let (used, frame) = right.receive(&prefix).unwrap();
        assert_eq!(used, prefix.len());
        assert_eq!(frame.as_deref(), Some(&b"left"[..]));
    }

    #[test]
    fn receive_stops_at_one_frame_and_preserves_the_remaining_stream() {
        let (mut sender, mut receiver) = pair();
        sender.prepare_send(b"first").unwrap();
        let mut wire = drain(&mut sender, usize::MAX);
        sender.prepare_send(b"second").unwrap();
        wire.extend_from_slice(&drain(&mut sender, usize::MAX));
        let (used, first) = receiver.receive(&wire).unwrap();
        assert_eq!(first.as_deref(), Some(&b"first"[..]));
        let (more, second) = receiver.receive(&wire[used..]).unwrap();
        assert_eq!(used + more, wire.len());
        assert_eq!(second.as_deref(), Some(&b"second"[..]));
    }

    #[test]
    fn invalid_tampered_and_truncated_records_close_the_channel() {
        let (_, mut receiver) = pair();
        assert!(matches!(
            receiver.receive(&[0, 1]),
            Err(Error::InvalidRecord)
        ));
        assert!(matches!(
            receiver.receive(&[]),
            Err(Error::Channel(ChannelError::Closed))
        ));

        let (_, mut receiver) = pair();
        assert_eq!(receiver.receive(&[0]).unwrap(), (1, None));
        assert!(matches!(receiver.end_input(), Err(Error::Truncated)));

        let (mut sender, mut receiver) = pair();
        sender.prepare_send(b"private").unwrap();
        let mut wire = drain(&mut sender, usize::MAX);
        *wire.last_mut().unwrap() ^= 1;
        assert!(matches!(
            receiver.receive(&wire),
            Err(Error::Channel(ChannelError::InvalidMessage))
        ));
        assert!(matches!(
            receiver.receive(&[]),
            Err(Error::Channel(ChannelError::Closed))
        ));

        let (mut sender, mut receiver) = pair();
        sender.prepare_send(&vec![7; MAX_MESSAGE_BYTES]).unwrap();
        let wire = drain(&mut sender, usize::MAX);
        let first_record_end = HEADER_BYTES + usize::from(u16::from_be_bytes([wire[0], wire[1]]));
        assert_eq!(
            receiver.receive(&wire[..first_record_end]).unwrap(),
            (first_record_end, None)
        );
        assert!(matches!(receiver.end_input(), Err(Error::Truncated)));
        assert!(matches!(
            receiver.receive(&[]),
            Err(Error::Channel(ChannelError::Closed))
        ));
    }

    #[test]
    fn record_io_requires_a_fresh_verified_channel() {
        let (mut sender, mut receiver) = channels();
        let records = sender.seal_frame(&vec![7; MAX_MESSAGE_BYTES]).unwrap();
        assert!(receiver.open_record(&records[0]).unwrap().is_none());
        assert!(matches!(
            RecordIo::new(receiver),
            Err(Error::Channel(ChannelError::InvalidMessage))
        ));
        assert!(matches!(
            RecordIo::new(sender),
            Err(Error::Channel(ChannelError::InvalidMessage))
        ));

        let (_, mut closed) = channels();
        closed.close();
        assert!(matches!(
            RecordIo::new(closed),
            Err(Error::Channel(ChannelError::Closed))
        ));
    }

    #[test]
    fn partial_write_failure_closes_without_reusing_cipher_state() {
        let (mut sender, _) = pair();
        sender.prepare_send(b"private").unwrap();
        assert_eq!(
            sender.write_pending(|_| Ok(1)).unwrap(),
            WriteProgress::Partial
        );
        assert!(matches!(
            sender.write_pending(|_| Err(io::ErrorKind::BrokenPipe.into())),
            Err(Error::Io(_))
        ));
        assert!(matches!(
            sender.prepare_send(b"retry"),
            Err(Error::Channel(ChannelError::Closed))
        ));

        let (mut sender, _) = pair();
        sender.prepare_send(b"private").unwrap();
        assert_eq!(
            sender
                .write_pending(|_| Err(io::ErrorKind::WouldBlock.into()))
                .unwrap(),
            WriteProgress::Blocked
        );
        sender.close();
        assert!(matches!(
            sender.write_pending(|_| panic!("cancelled output must not be written")),
            Err(Error::Channel(ChannelError::Closed))
        ));

        let (mut sender, _) = pair();
        sender.prepare_send(b"private").unwrap();
        assert!(matches!(sender.end_input(), Err(Error::Truncated)));

        let (mut sender, _) = pair();
        sender.prepare_send(b"private").unwrap();
        assert!(matches!(
            sender.write_pending(|pending| Ok(pending.len() + 1)),
            Err(Error::InvalidRecord)
        ));
        assert!(matches!(
            sender.prepare_send(b"retry"),
            Err(Error::Channel(ChannelError::Closed))
        ));
    }
}
