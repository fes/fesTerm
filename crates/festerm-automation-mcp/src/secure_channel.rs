//! An unverified, encrypted channel primitive for the future local adapter.
//!
//! Noise NN does not authenticate either endpoint. Both endpoints must derive
//! and independently present the complete fingerprint for trusted human
//! comparison before a caller is trusted or any sensitive data is exchanged.
//! The handshake I/O driver does not implement that comparison, approval,
//! deadlines, privacy monitoring, or process identity, and does not enable MCP.
//! Raw handshake construction is private so the driver cannot be bypassed.
//!
//! ```compile_fail
//! use festerm_automation_mcp::secure_channel;
//! let _ = secure_channel::respond(&[0; 32]);
//! ```
//!
//! ```compile_fail
//! use festerm_automation_mcp::secure_channel;
//! let _ = secure_channel::initiate();
//! ```
//!
//! An unverified channel cannot encrypt application data:
//!
//! ```compile_fail
//! use festerm_automation_mcp::secure_channel::UnverifiedChannel;
//! fn send(channel: &mut UnverifiedChannel) {
//!     let _ = channel.seal_frame(b"private");
//! }
//! ```

use sha2::{Digest, Sha256};
use snow::{
    params::{CipherChoice, DHChoice, HashChoice},
    resolvers::{CryptoResolver, DefaultResolver},
    types::{Cipher, Dh, Hash, Random},
};
use std::fmt;
use subtle::ConstantTimeEq;

use crate::MAX_MESSAGE_BYTES;

#[path = "secure_handshake_io.rs"]
pub mod handshake_io;

const PATTERN: &str = "Noise_NN_25519_ChaChaPoly_SHA256";
const PROLOGUE: &[u8] = b"fesTerm local MCP channel v1; initiator=adapter; responder=application";
const FINGERPRINT_DOMAIN: &[u8] = b"fesTerm local MCP channel fingerprint v1\0";
const FIRST_HANDSHAKE_BYTES: usize = 32;
const SECOND_HANDSHAKE_BYTES: usize = 48;
const TAG_BYTES: usize = 16;
pub const MAX_RECORD_BYTES: usize = 65_535;
pub const MIN_RECORD_BYTES: usize = TAG_BYTES + 1;
const MAX_RECORD_PLAINTEXT: usize = MAX_RECORD_BYTES - TAG_BYTES;
const HEADER_BYTES: usize = 4;

/// Content-free channel failures; callers must close the owned connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelError {
    Unavailable,
    InvalidMessage,
    Closed,
}

impl fmt::Display for ChannelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "secure channel unavailable",
            Self::InvalidMessage => "secure channel message refused",
            Self::Closed => "secure channel closed",
        })
    }
}

impl std::error::Error for ChannelError {}

/// Public channel binding, not a credential or proof of executable identity.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Fingerprint([u8; 32]);

impl fmt::Display for Fingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

struct Initiator(snow::HandshakeState);

struct CheckedResolver;

impl CryptoResolver for CheckedResolver {
    fn resolve_rng(&self) -> Option<Box<dyn Random>> {
        DefaultResolver.resolve_rng()
    }

    fn resolve_dh(&self, choice: &DHChoice) -> Option<Box<dyn Dh>> {
        DefaultResolver
            .resolve_dh(choice)
            .map(|inner| Box::new(CheckedDh(inner)) as Box<dyn Dh>)
    }

    fn resolve_hash(&self, choice: &HashChoice) -> Option<Box<dyn Hash>> {
        DefaultResolver.resolve_hash(choice)
    }

    fn resolve_cipher(&self, choice: &CipherChoice) -> Option<Box<dyn Cipher>> {
        DefaultResolver.resolve_cipher(choice)
    }
}

struct CheckedDh(Box<dyn Dh>);

// Snow's default X25519 resolver does not reject an all-zero shared secret.
impl Dh for CheckedDh {
    fn name(&self) -> &'static str {
        self.0.name()
    }

    fn pub_len(&self) -> usize {
        self.0.pub_len()
    }

    fn priv_len(&self) -> usize {
        self.0.priv_len()
    }

    fn dh_len(&self) -> usize {
        self.0.dh_len()
    }

    fn set(&mut self, privkey: &[u8]) {
        self.0.set(privkey);
    }

    fn generate(&mut self, rng: &mut dyn Random) -> Result<(), snow::Error> {
        self.0.generate(rng)
    }

    fn pubkey(&self) -> &[u8] {
        self.0.pubkey()
    }

    fn privkey(&self) -> &[u8] {
        self.0.privkey()
    }

    fn dh(&self, pubkey: &[u8], out: &mut [u8]) -> Result<(), snow::Error> {
        if self.0.dh_len() != 32 || out.len() < 32 {
            return Err(snow::Error::Dh);
        }
        self.0.dh(pubkey, out)?;
        if bool::from(out[..32].ct_eq(&[0; 32][..])) {
            return Err(snow::Error::Dh);
        }
        Ok(())
    }
}

fn handshake(initiator: bool) -> Result<snow::HandshakeState, ChannelError> {
    let parameters = PATTERN.parse().map_err(|_| ChannelError::Unavailable)?;
    let builder = snow::Builder::with_resolver(parameters, Box::new(CheckedResolver))
        .prologue(PROLOGUE)
        .map_err(|_| ChannelError::Unavailable)?;
    if initiator {
        builder.build_initiator()
    } else {
        builder.build_responder()
    }
    .map_err(|_| ChannelError::Unavailable)
}

fn initiate() -> Result<(Initiator, Vec<u8>), ChannelError> {
    let mut state = handshake(true)?;
    let mut first = vec![0; FIRST_HANDSHAKE_BYTES + TAG_BYTES];
    let length = state
        .write_message(&[], &mut first)
        .map_err(|_| ChannelError::Unavailable)?;
    if length != FIRST_HANDSHAKE_BYTES {
        return Err(ChannelError::Unavailable);
    }
    first.truncate(length);
    Ok((Initiator(state), first))
}

impl Initiator {
    fn finish(mut self, reply: &[u8]) -> Result<UnverifiedChannel, ChannelError> {
        if reply.len() != SECOND_HANDSHAKE_BYTES {
            return Err(ChannelError::InvalidMessage);
        }
        let mut payload = [0; 1];
        let length = self
            .0
            .read_message(reply, &mut payload)
            .map_err(|_| ChannelError::InvalidMessage)?;
        if length != 0 {
            return Err(ChannelError::InvalidMessage);
        }
        finish(self.0)
    }
}

fn respond(first: &[u8]) -> Result<(UnverifiedChannel, Vec<u8>), ChannelError> {
    if first.len() != FIRST_HANDSHAKE_BYTES {
        return Err(ChannelError::InvalidMessage);
    }
    let mut state = handshake(false)?;
    let mut payload = [0; 1];
    let length = state
        .read_message(first, &mut payload)
        .map_err(|_| ChannelError::InvalidMessage)?;
    if length != 0 {
        return Err(ChannelError::InvalidMessage);
    }
    let mut reply = vec![0; SECOND_HANDSHAKE_BYTES];
    let length = state
        .write_message(&[], &mut reply)
        .map_err(|_| ChannelError::InvalidMessage)?;
    if length != SECOND_HANDSHAKE_BYTES {
        return Err(ChannelError::InvalidMessage);
    }
    Ok((finish(state)?, reply))
}

fn finish(state: snow::HandshakeState) -> Result<UnverifiedChannel, ChannelError> {
    if !state.is_handshake_finished() {
        return Err(ChannelError::InvalidMessage);
    }
    let mut digest = Sha256::new();
    digest.update(FINGERPRINT_DOMAIN);
    digest.update(state.get_handshake_hash());
    let digest = digest.finalize();
    let fingerprint = Fingerprint(digest.into());
    let transport = state
        .into_transport_mode()
        .map_err(|_| ChannelError::InvalidMessage)?;
    Ok(UnverifiedChannel {
        transport,
        fingerprint,
    })
}

/// An encrypted session that has not yet been authenticated by the user.
/// It deliberately cannot encrypt or decrypt application frames.
pub struct UnverifiedChannel {
    transport: snow::TransportState,
    fingerprint: Fingerprint,
}

impl UnverifiedChannel {
    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    /// Confirm only a complete fingerprint supplied through an independently
    /// trusted human comparison, never this endpoint's own value or a value
    /// received over the peer channel being verified. The caller owns that
    /// trust boundary; equality alone is not proof of a human comparison.
    pub fn confirm(self, compared_fingerprint: &str) -> Result<Channel, ChannelError> {
        if self.fingerprint.to_string() != compared_fingerprint {
            return Err(ChannelError::InvalidMessage);
        }
        Ok(Channel {
            transport: Some(self.transport),
            fingerprint: self.fingerprint,
            pending: None,
            used: false,
        })
    }
}

struct PendingFrame {
    total: usize,
    bytes: Vec<u8>,
}

/// Stateful ordered encryption, not an authenticated principal or native grant.
///
/// The caller must bound record I/O and close this channel on any failed or
/// partial write. Records from one frame must be sent in order without
/// interleaving; plaintext is returned only after the whole frame is verified.
pub struct Channel {
    transport: Option<snow::TransportState>,
    fingerprint: Fingerprint,
    pending: Option<PendingFrame>,
    used: bool,
}

impl Channel {
    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    pub fn close(&mut self) {
        self.transport = None;
        self.pending = None;
    }

    pub(crate) fn require_fresh(&self) -> Result<(), ChannelError> {
        if self.transport.is_none() {
            Err(ChannelError::Closed)
        } else if self.used {
            Err(ChannelError::InvalidMessage)
        } else {
            Ok(())
        }
    }

    fn fail<T>(&mut self, error: ChannelError) -> Result<T, ChannelError> {
        self.close();
        Err(error)
    }

    /// Encrypt one bounded application frame as one or two ordered records.
    pub fn seal_frame(&mut self, frame: &[u8]) -> Result<Vec<Vec<u8>>, ChannelError> {
        if self.transport.is_none() {
            return Err(ChannelError::Closed);
        }
        self.used = true;
        if frame.is_empty() || frame.len() > MAX_MESSAGE_BYTES {
            return self.fail(ChannelError::InvalidMessage);
        }
        let mut plaintext = Vec::with_capacity(HEADER_BYTES + frame.len());
        plaintext.extend_from_slice(&(frame.len() as u32).to_be_bytes());
        plaintext.extend_from_slice(frame);
        let mut records = Vec::with_capacity(2);
        for chunk in plaintext.chunks(MAX_RECORD_PLAINTEXT) {
            let mut ciphertext = vec![0; chunk.len() + TAG_BYTES];
            let Some(state) = self.transport.as_mut() else {
                return Err(ChannelError::Closed);
            };
            match state.write_message(chunk, &mut ciphertext) {
                Ok(length) if length == ciphertext.len() => records.push(ciphertext),
                _ => return self.fail(ChannelError::InvalidMessage),
            }
        }
        Ok(records)
    }

    /// Authenticate one ordered record, returning a frame only when complete.
    pub fn open_record(&mut self, ciphertext: &[u8]) -> Result<Option<Vec<u8>>, ChannelError> {
        if self.transport.is_none() {
            return Err(ChannelError::Closed);
        }
        self.used = true;
        if !(MIN_RECORD_BYTES..=MAX_RECORD_BYTES).contains(&ciphertext.len()) {
            return self.fail(ChannelError::InvalidMessage);
        }
        let mut plaintext = vec![0; ciphertext.len()];
        let Some(state) = self.transport.as_mut() else {
            return Err(ChannelError::Closed);
        };
        let length = match state.read_message(ciphertext, &mut plaintext) {
            Ok(length) => length,
            Err(_) => return self.fail(ChannelError::InvalidMessage),
        };
        plaintext.truncate(length);
        if let Some(mut pending) = self.pending.take() {
            if plaintext.len() != pending.total - pending.bytes.len() {
                return self.fail(ChannelError::InvalidMessage);
            }
            pending.bytes.extend_from_slice(&plaintext);
            return Ok(Some(pending.bytes));
        }
        if plaintext.len() < HEADER_BYTES {
            return self.fail(ChannelError::InvalidMessage);
        }
        let total = u32::from_be_bytes(
            plaintext[..HEADER_BYTES]
                .try_into()
                .map_err(|_| ChannelError::InvalidMessage)?,
        ) as usize;
        if total == 0 || total > MAX_MESSAGE_BYTES {
            return self.fail(ChannelError::InvalidMessage);
        }
        let first = &plaintext[HEADER_BYTES..];
        if total <= MAX_RECORD_PLAINTEXT - HEADER_BYTES {
            if first.len() != total {
                return self.fail(ChannelError::InvalidMessage);
            }
            return Ok(Some(first.to_vec()));
        }
        if plaintext.len() != MAX_RECORD_PLAINTEXT {
            return self.fail(ChannelError::InvalidMessage);
        }
        self.pending = Some(PendingFrame {
            total,
            bytes: first.to_vec(),
        });
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unverified_pair() -> (UnverifiedChannel, UnverifiedChannel) {
        let (initiator, first) = initiate().unwrap();
        let (responder, reply) = respond(&first).unwrap();
        (initiator.finish(&reply).unwrap(), responder)
    }

    fn pair() -> (Channel, Channel) {
        let (adapter, application) = unverified_pair();
        let adapter_code = adapter.fingerprint().to_string();
        let application_code = application.fingerprint().to_string();
        (
            adapter.confirm(&application_code).unwrap(),
            application.confirm(&adapter_code).unwrap(),
        )
    }

    #[test]
    fn channel_fingerprint_is_independently_derived_and_frames_are_bounded() {
        let (mut adapter, mut application) = pair();
        assert_eq!(
            adapter.fingerprint().to_string(),
            application.fingerprint().to_string()
        );
        assert_eq!(adapter.fingerprint().to_string().len(), 64);
        for size in [1, MAX_RECORD_PLAINTEXT - HEADER_BYTES, MAX_MESSAGE_BYTES] {
            let frame = vec![42; size];
            let records = adapter.seal_frame(&frame).unwrap();
            assert!(records.len() <= 2);
            let mut result = None;
            for record in records {
                assert!(record.len() <= MAX_RECORD_BYTES);
                result = application.open_record(&record).unwrap();
            }
            assert_eq!(result, Some(frame));
        }
        let response = application.seal_frame(b"reply").unwrap();
        assert_eq!(
            adapter.open_record(&response[0]).unwrap(),
            Some(b"reply".to_vec())
        );
    }

    #[test]
    fn split_handshakes_have_different_fingerprints_and_cannot_replay() {
        let (mut adapter, mut attacker_as_application) = pair();
        let (mut attacker_as_adapter, mut application) = pair();
        assert_ne!(
            adapter.fingerprint().to_string(),
            application.fingerprint().to_string()
        );
        let record = adapter.seal_frame(b"private").unwrap().remove(0);
        let plaintext = attacker_as_application
            .open_record(&record)
            .unwrap()
            .unwrap();
        let forwarded = attacker_as_adapter
            .seal_frame(&plaintext)
            .unwrap()
            .remove(0);
        assert_eq!(
            application.open_record(&forwarded).unwrap(),
            Some(b"private".to_vec())
        );
        assert_eq!(
            application.open_record(&forwarded),
            Err(ChannelError::InvalidMessage)
        );
        assert_eq!(
            application.open_record(&forwarded),
            Err(ChannelError::Closed)
        );
    }

    #[test]
    fn tampering_reflection_and_out_of_order_records_permanently_close() {
        let (mut adapter, mut application) = pair();
        let records = adapter.seal_frame(&vec![1; MAX_MESSAGE_BYTES]).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(
            application.open_record(&records[1]),
            Err(ChannelError::InvalidMessage)
        );
        assert_eq!(
            application.open_record(&records[0]),
            Err(ChannelError::Closed)
        );

        let (mut adapter, mut application) = pair();
        let mut record = adapter.seal_frame(b"private").unwrap().remove(0);
        assert_eq!(
            adapter.open_record(&record),
            Err(ChannelError::InvalidMessage)
        );
        record[0] ^= 1;
        assert_eq!(
            application.open_record(&record),
            Err(ChannelError::InvalidMessage)
        );
        assert_eq!(application.open_record(&record), Err(ChannelError::Closed));
    }

    #[test]
    fn invalid_lengths_and_malformed_handshakes_fail_closed() {
        assert!(matches!(
            respond(&[0; 31]),
            Err(ChannelError::InvalidMessage)
        ));
        assert!(matches!(
            respond(&[0; 32]),
            Err(ChannelError::InvalidMessage)
        ));
        let mut low_order = [0; 32];
        low_order[0] = 1;
        assert!(matches!(
            respond(&low_order),
            Err(ChannelError::InvalidMessage)
        ));
        let (initiator, _) = initiate().unwrap();
        assert!(matches!(
            initiator.finish(&[0; 49]),
            Err(ChannelError::InvalidMessage)
        ));
        let (mut adapter, mut application) = pair();
        assert_eq!(adapter.seal_frame(&[]), Err(ChannelError::InvalidMessage));
        assert_eq!(adapter.seal_frame(b"valid"), Err(ChannelError::Closed));
        assert_eq!(
            application.open_record(&[0; TAG_BYTES]),
            Err(ChannelError::InvalidMessage)
        );
        assert_eq!(application.seal_frame(b"valid"), Err(ChannelError::Closed));
    }

    #[test]
    fn verification_requires_the_complete_independent_fingerprint() {
        let (adapter, application) = unverified_pair();
        let full = application.fingerprint().to_string();
        assert!(matches!(
            adapter.confirm(&full[..full.len() - 1]),
            Err(ChannelError::InvalidMessage)
        ));

        let (adapter, application) = unverified_pair();
        let mut wrong = application.fingerprint().to_string().into_bytes();
        wrong[0] = if wrong[0] == b'0' { b'1' } else { b'0' };
        assert!(matches!(
            adapter.confirm(std::str::from_utf8(&wrong).unwrap()),
            Err(ChannelError::InvalidMessage)
        ));
    }

    #[test]
    fn fresh_handshakes_reject_old_responses_and_ciphertexts() {
        let (initiator, first) = initiate().unwrap();
        let (old_application, old_reply) = respond(&first).unwrap();
        let old_adapter = initiator.finish(&old_reply).unwrap();
        let old_code = old_adapter.fingerprint().to_string();
        let mut old_adapter = old_adapter
            .confirm(&old_application.fingerprint().to_string())
            .unwrap();
        let old_record = old_adapter.seal_frame(b"old").unwrap().remove(0);

        let (fresh_application, fresh_reply) = respond(&first).unwrap();
        assert_ne!(old_code, fresh_application.fingerprint().to_string());
        assert_ne!(old_reply, fresh_reply);
        let (fresh_initiator, _) = initiate().unwrap();
        assert!(matches!(
            fresh_initiator.finish(&old_reply),
            Err(ChannelError::InvalidMessage)
        ));
        let (_, mut new_application) = pair();
        assert_eq!(
            new_application.open_record(&old_record),
            Err(ChannelError::InvalidMessage)
        );
    }

    #[test]
    fn malformed_authenticated_frame_headers_close_without_disclosure() {
        for plaintext in [
            vec![0, 0, 0, 0],
            vec![0, 0, 0, 1, b'a', b'b'],
            vec![0, 1, 0, 0, b'a'],
        ] {
            let (mut adapter, mut application) = pair();
            let mut ciphertext = vec![0; plaintext.len() + TAG_BYTES];
            adapter
                .transport
                .as_mut()
                .unwrap()
                .write_message(&plaintext, &mut ciphertext)
                .unwrap();
            assert_eq!(
                application.open_record(&ciphertext),
                Err(ChannelError::InvalidMessage)
            );
            assert_eq!(
                application.open_record(&ciphertext),
                Err(ChannelError::Closed)
            );
        }
    }
}
