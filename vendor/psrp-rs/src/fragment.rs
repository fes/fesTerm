//! PSRP fragment layer (MS-PSRP §2.2.4).
//!
//! A PSRP message is sliced into one or more fragments. Each fragment has a
//! 21-byte big-endian header followed by a blob:
//!
//! ```text
//!  0                               8                              16
//! +-------------------------------+-------------------------------+---+-----+
//! |          ObjectId  (u64 BE)   |        FragmentId (u64 BE)    | F | Len |
//! +-------------------------------+-------------------------------+---+-----+
//!                                                                  ^17 ^21 + payload
//! ```
//!
//! `F` flags: `0x01` = Start of object, `0x02` = End of object.
//!
//! The fragmenter splits a message payload at [`MAX_FRAGMENT_PAYLOAD`]; the
//! reassembler is stateful and tolerates fragments being split at arbitrary
//! byte boundaries across successive `feed` calls.

use std::collections::HashMap;

use crate::error::{PsrpError, Result};

/// Maximum payload bytes per fragment. Matches `pypsrp`'s default and is well
/// under `winrm-rs`' default `max_envelope_size` of 153 600 bytes (base64
/// overhead included).
pub const MAX_FRAGMENT_PAYLOAD: usize = 32 * 1024;

/// Size of the fragment header in bytes.
pub const FRAGMENT_HEADER_LEN: usize = 21;

const FLAG_START: u8 = 0x01;
const FLAG_END: u8 = 0x02;
const VALID_FLAGS: u8 = FLAG_START | FLAG_END;

/// Hard limits applied while reassembling untrusted fragment streams.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ReassemblyLimits {
    /// Maximum bytes accepted in a single `feed` call.
    pub max_feed_bytes: usize,
    /// Maximum bytes retained in the partial-fragment buffer.
    pub max_buffered_bytes: usize,
    /// Maximum number of partially assembled objects.
    pub max_in_flight_messages: usize,
    /// Maximum fragments accepted for a single message.
    pub max_fragments_per_message: usize,
    /// Maximum assembled payload bytes for one message.
    pub max_message_bytes: usize,
    /// Maximum bytes retained across all in-flight messages.
    pub max_total_in_flight_bytes: usize,
    /// Maximum complete messages returned from one `feed` call.
    pub max_completed_messages_per_feed: usize,
}

impl Default for ReassemblyLimits {
    fn default() -> Self {
        Self {
            max_feed_bytes: 512 * 1024,
            max_buffered_bytes: 512 * 1024,
            max_in_flight_messages: 32,
            max_fragments_per_message: 128,
            max_message_bytes: 4 * 1024 * 1024,
            max_total_in_flight_bytes: 8 * 1024 * 1024,
            max_completed_messages_per_feed: 256,
        }
    }
}

/// A single PSRP fragment (header + blob).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    /// Object identifier shared by all fragments of the same message.
    pub object_id: u64,
    /// 0-based index of this fragment within the message.
    pub fragment_id: u64,
    /// True for the first fragment of an object.
    pub start: bool,
    /// True for the last fragment of an object.
    pub end: bool,
    /// Payload bytes for this fragment.
    pub blob: Vec<u8>,
}

impl Fragment {
    fn flags(&self) -> u8 {
        let mut f = 0;
        if self.start {
            f |= FLAG_START;
        }
        if self.end {
            f |= FLAG_END;
        }
        f
    }

    /// Serialize this fragment to its wire representation.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(FRAGMENT_HEADER_LEN + self.blob.len());
        out.extend_from_slice(&self.object_id.to_be_bytes());
        out.extend_from_slice(&self.fragment_id.to_be_bytes());
        out.push(self.flags());
        out.extend_from_slice(
            &u32::try_from(self.blob.len())
                .unwrap_or(u32::MAX)
                .to_be_bytes(),
        );
        out.extend_from_slice(&self.blob);
        out
    }
}

/// Split a complete PSRP message payload into fragments for the given
/// `object_id`. At least one fragment is always returned (an empty message
/// yields a single `start+end` fragment with an empty blob).
#[must_use]
pub fn split_message(object_id: u64, payload: &[u8]) -> Vec<Fragment> {
    if payload.is_empty() {
        return vec![Fragment {
            object_id,
            fragment_id: 0,
            start: true,
            end: true,
            blob: Vec::new(),
        }];
    }

    let mut out = Vec::new();
    let chunks: Vec<&[u8]> = payload.chunks(MAX_FRAGMENT_PAYLOAD).collect();
    let last = chunks.len() - 1;
    for (i, chunk) in chunks.into_iter().enumerate() {
        out.push(Fragment {
            object_id,
            fragment_id: i as u64,
            start: i == 0,
            end: i == last,
            blob: chunk.to_vec(),
        });
    }
    out
}

/// Encode every fragment produced by [`split_message`] into a single
/// concatenated byte buffer ready to be sent via `Shell::send_input`.
#[must_use]
pub fn encode_message(object_id: u64, payload: &[u8]) -> Vec<u8> {
    let frags = split_message(object_id, payload);
    let total: usize = frags
        .iter()
        .map(|f| FRAGMENT_HEADER_LEN + f.blob.len())
        .sum();
    let mut out = Vec::with_capacity(total);
    for f in frags {
        out.extend_from_slice(&f.encode());
    }
    out
}

/// Track partial messages so they can be emitted whole once the final
/// fragment arrives.
#[derive(Debug, Default)]
struct InFlight {
    buf: Vec<u8>,
    next_fragment_id: u64,
    started: bool,
    fragment_count: usize,
}

/// Stateful reassembler for incoming PSRP fragments.
///
/// Bytes from `Shell::receive_next` can be fed in arbitrary chunks. Whenever
/// a complete message is reconstructed (from the `start` fragment through the
/// `end` fragment), its payload is returned in order from [`Reassembler::feed`].
#[derive(Debug)]
pub struct Reassembler {
    buffer: Vec<u8>,
    in_flight: HashMap<u64, InFlight>,
    completed_order: Vec<u64>,
    limits: ReassemblyLimits,
    total_in_flight_bytes: usize,
}

impl Default for Reassembler {
    fn default() -> Self {
        Self::new_with_limits(ReassemblyLimits::default())
    }
}

impl Reassembler {
    /// Create a fresh reassembler with default limits.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a reassembler with explicit bounds.
    #[must_use]
    pub fn new_with_limits(limits: ReassemblyLimits) -> Self {
        Self {
            buffer: Vec::new(),
            in_flight: HashMap::new(),
            completed_order: Vec::new(),
            limits,
            total_in_flight_bytes: 0,
        }
    }

    /// Return the active limits.
    #[must_use]
    pub fn limits(&self) -> ReassemblyLimits {
        self.limits
    }

    /// Feed raw bytes received from the transport and return every message
    /// payload that becomes complete as a result.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
        if bytes.len() > self.limits.max_feed_bytes {
            return Err(PsrpError::fragment(format!(
                "feed size {} exceeds {} bytes",
                bytes.len(),
                self.limits.max_feed_bytes
            )));
        }
        if self.buffer.len().saturating_add(bytes.len()) > self.limits.max_buffered_bytes {
            return Err(PsrpError::fragment(format!(
                "buffered fragment bytes exceed {}",
                self.limits.max_buffered_bytes
            )));
        }
        self.buffer.extend_from_slice(bytes);
        let mut completed = Vec::new();

        loop {
            if self.buffer.len() < FRAGMENT_HEADER_LEN {
                break;
            }
            let header = &self.buffer[..FRAGMENT_HEADER_LEN];
            let object_id = u64::from_be_bytes(header[0..8].try_into().unwrap());
            let fragment_id = u64::from_be_bytes(header[8..16].try_into().unwrap());
            let flags = header[16];
            let blob_len = u32::from_be_bytes(header[17..21].try_into().unwrap()) as usize;

            if flags & !VALID_FLAGS != 0 {
                return Err(PsrpError::fragment(format!(
                    "invalid fragment flags 0x{flags:02X} for object {object_id}"
                )));
            }
            if blob_len > self.limits.max_message_bytes {
                return Err(PsrpError::fragment(format!(
                    "fragment payload {blob_len} exceeds {} bytes",
                    self.limits.max_message_bytes
                )));
            }
            if self.buffer.len() < FRAGMENT_HEADER_LEN + blob_len {
                break; // need more bytes
            }

            let start = flags & FLAG_START != 0;
            let end = flags & FLAG_END != 0;

            let blob_start = FRAGMENT_HEADER_LEN;
            let blob_end = blob_start + blob_len;
            let blob: Vec<u8> = self.buffer[blob_start..blob_end].to_vec();
            self.buffer.drain(..blob_end);

            if !self.in_flight.contains_key(&object_id) {
                if self.in_flight.len() >= self.limits.max_in_flight_messages {
                    return Err(PsrpError::fragment(format!(
                        "in-flight message count exceeds {}",
                        self.limits.max_in_flight_messages
                    )));
                }
            }
            let entry = self.in_flight.entry(object_id).or_default();

            if start {
                if entry.started {
                    return Err(PsrpError::fragment(format!(
                        "duplicate start fragment for object {object_id}"
                    )));
                }
                if fragment_id != 0 {
                    return Err(PsrpError::fragment(format!(
                        "start fragment for object {object_id} has non-zero fragment id {fragment_id}"
                    )));
                }
                entry.started = true;
                entry.next_fragment_id = 0;
            } else if !entry.started {
                return Err(PsrpError::fragment(format!(
                    "continuation fragment before start for object {object_id}"
                )));
            }

            if fragment_id != entry.next_fragment_id {
                return Err(PsrpError::fragment(format!(
                    "out-of-order fragment for object {object_id}: expected {}, got {fragment_id}",
                    entry.next_fragment_id
                )));
            }
            if entry.fragment_count >= self.limits.max_fragments_per_message {
                return Err(PsrpError::fragment(format!(
                    "object {object_id} exceeded {} fragments",
                    self.limits.max_fragments_per_message
                )));
            }
            if entry.buf.len().saturating_add(blob_len) > self.limits.max_message_bytes {
                return Err(PsrpError::fragment(format!(
                    "object {object_id} exceeded {} assembled bytes",
                    self.limits.max_message_bytes
                )));
            }
            if self.total_in_flight_bytes.saturating_add(blob_len)
                > self.limits.max_total_in_flight_bytes
            {
                return Err(PsrpError::fragment(format!(
                    "in-flight fragment bytes exceed {}",
                    self.limits.max_total_in_flight_bytes
                )));
            }

            entry.next_fragment_id += 1;
            entry.fragment_count += 1;
            entry.buf.extend_from_slice(&blob);
            self.total_in_flight_bytes += blob_len;

            if end {
                if completed.len() >= self.limits.max_completed_messages_per_feed {
                    return Err(PsrpError::fragment(format!(
                        "completed messages per feed exceed {}",
                        self.limits.max_completed_messages_per_feed
                    )));
                }
                let done = self.in_flight.remove(&object_id).unwrap().buf;
                self.total_in_flight_bytes = self.total_in_flight_bytes.saturating_sub(done.len());
                completed.push(done);
                self.completed_order.push(object_id);
            }
        }

        Ok(completed)
    }

    /// True if there are no partially-accumulated messages and no leftover
    /// buffered bytes.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.buffer.is_empty() && self.in_flight.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_roundtrip_single_fragment() {
        let payload = b"hello world".to_vec();
        let bytes = encode_message(42, &payload);
        let mut r = Reassembler::new();
        let out = r.feed(&bytes).unwrap();
        assert_eq!(out, vec![payload]);
        assert!(r.is_idle());
    }

    #[test]
    fn empty_message_roundtrip() {
        let bytes = encode_message(7, b"");
        let mut r = Reassembler::new();
        let out = r.feed(&bytes).unwrap();
        assert_eq!(out, vec![Vec::<u8>::new()]);
    }

    #[test]
    fn splits_at_max_fragment_payload() {
        let payload = vec![0xABu8; MAX_FRAGMENT_PAYLOAD * 2 + 10];
        let frags = split_message(1, &payload);
        assert_eq!(frags.len(), 3);
        assert!(frags[0].start && !frags[0].end);
        assert!(!frags[1].start && !frags[1].end);
        assert!(!frags[2].start && frags[2].end);
        assert_eq!(frags[0].blob.len(), MAX_FRAGMENT_PAYLOAD);
        assert_eq!(frags[1].blob.len(), MAX_FRAGMENT_PAYLOAD);
        assert_eq!(frags[2].blob.len(), 10);
    }

    #[test]
    fn fragmented_across_multiple_feeds() {
        let payload = vec![b'x'; MAX_FRAGMENT_PAYLOAD + 3];
        let encoded = encode_message(99, &payload);
        let mut r = Reassembler::new();

        let cut1 = 10;
        let cut2 = MAX_FRAGMENT_PAYLOAD + FRAGMENT_HEADER_LEN + 5;
        assert!(r.feed(&encoded[..cut1]).unwrap().is_empty());
        assert!(r.feed(&encoded[cut1..cut2]).unwrap().is_empty());
        let out = r.feed(&encoded[cut2..]).unwrap();
        assert_eq!(out, vec![payload]);
        assert!(r.is_idle());
    }

    #[test]
    fn rejects_duplicate_start() {
        let f1 = Fragment {
            object_id: 1,
            fragment_id: 0,
            start: true,
            end: false,
            blob: b"abc".to_vec(),
        }
        .encode();
        let f2 = Fragment {
            object_id: 1,
            fragment_id: 0,
            start: true,
            end: true,
            blob: b"def".to_vec(),
        }
        .encode();

        let mut r = Reassembler::new();
        assert!(r.feed(&f1).is_ok());
        let err = r.feed(&f2).unwrap_err();
        assert!(err.to_string().contains("duplicate start"));
    }

    #[test]
    fn rejects_out_of_order_fragment() {
        let start = Fragment {
            object_id: 2,
            fragment_id: 0,
            start: true,
            end: false,
            blob: b"a".to_vec(),
        }
        .encode();
        let bad = Fragment {
            object_id: 2,
            fragment_id: 2,
            start: false,
            end: true,
            blob: b"b".to_vec(),
        }
        .encode();

        let mut r = Reassembler::new();
        assert!(r.feed(&start).is_ok());
        let err = r.feed(&bad).unwrap_err();
        assert!(err.to_string().contains("out-of-order"));
    }

    #[test]
    fn rejects_excessive_feed_bytes_before_buffer_growth() {
        let mut r = Reassembler::new_with_limits(ReassemblyLimits {
            max_feed_bytes: FRAGMENT_HEADER_LEN,
            ..ReassemblyLimits::default()
        });
        let err = r.feed(&[0u8; FRAGMENT_HEADER_LEN + 1]).unwrap_err();
        assert!(err.to_string().contains("feed size"));
        assert!(r.is_idle());
    }

    #[test]
    fn rejects_excessive_assembled_bytes() {
        let payload = vec![0xAA; 12];
        let bytes = encode_message(1, &payload);
        let mut r = Reassembler::new_with_limits(ReassemblyLimits {
            max_message_bytes: 8,
            ..ReassemblyLimits::default()
        });
        let err = r.feed(&bytes).unwrap_err();
        assert!(
            err.to_string().contains("assembled bytes")
                || err.to_string().contains("fragment payload")
        );
    }

    #[test]
    fn rejects_too_many_in_flight_messages() {
        let first = Fragment {
            object_id: 1,
            fragment_id: 0,
            start: true,
            end: false,
            blob: b"a".to_vec(),
        }
        .encode();
        let second = Fragment {
            object_id: 2,
            fragment_id: 0,
            start: true,
            end: false,
            blob: b"b".to_vec(),
        }
        .encode();
        let mut r = Reassembler::new_with_limits(ReassemblyLimits {
            max_in_flight_messages: 1,
            ..ReassemblyLimits::default()
        });
        assert!(r.feed(&first).unwrap().is_empty());
        let err = r.feed(&second).unwrap_err();
        assert!(err.to_string().contains("in-flight message count"));
    }

    #[test]
    fn rejects_unknown_flag_bits() {
        let mut bytes = Fragment {
            object_id: 1,
            fragment_id: 0,
            start: true,
            end: true,
            blob: b"x".to_vec(),
        }
        .encode();
        bytes[16] |= 0x80;
        let mut r = Reassembler::new();
        let err = r.feed(&bytes).unwrap_err();
        assert!(err.to_string().contains("invalid fragment flags"));
    }
}
