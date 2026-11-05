use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

const IDENTITY_DOMAIN: &[u8] = b"fesTerm MCP client authentication v1\0";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthChallenge {
    version: u8,
    nonce: String,
}

impl AuthChallenge {
    #[must_use]
    pub fn new(nonce: [u8; 32]) -> Self {
        Self {
            version: 1,
            nonce: encode_hex(&nonce),
        }
    }

    fn nonce_bytes(&self) -> Result<[u8; 32], IdentityError> {
        if self.version != 1 {
            return Err(IdentityError::UnsupportedVersion);
        }
        decode_hex::<32>(&self.nonce).ok_or(IdentityError::InvalidEncoding)
    }

    fn message(&self) -> Result<Vec<u8>, IdentityError> {
        let nonce = self.nonce_bytes()?;
        let mut message = Vec::with_capacity(IDENTITY_DOMAIN.len() + nonce.len() + 1);
        message.extend_from_slice(IDENTITY_DOMAIN);
        message.push(self.version);
        message.extend_from_slice(&nonce);
        Ok(message)
    }
}

/// A client-owned Ed25519 signing key. Keep the seed out of argv and logs.
pub struct ClientIdentity(SigningKey);

impl ClientIdentity {
    /// Generates a seed for an MCP client configuration. The returned value is
    /// secret material and must not be logged or passed as a command-line argument.
    pub fn generate_seed_hex() -> Result<String, IdentityError> {
        let mut seed = [0; 32];
        getrandom::fill(&mut seed).map_err(|_| IdentityError::RandomUnavailable)?;
        let encoded = encode_hex(&seed);
        zeroize::Zeroize::zeroize(&mut seed);
        Ok(encoded)
    }

    /// Reads a canonical lowercase hex encoding of a 32-byte Ed25519 seed.
    pub fn from_hex_seed(seed: &str) -> Result<Self, IdentityError> {
        let mut bytes = decode_hex::<32>(seed).ok_or(IdentityError::InvalidEncoding)?;
        let identity = Self(SigningKey::from_bytes(&bytes));
        zeroize::Zeroize::zeroize(&mut bytes);
        Ok(identity)
    }

    #[must_use]
    pub fn proof(&self, challenge: &AuthChallenge) -> Result<ClientIdentityProof, IdentityError> {
        let signature = self.0.sign(&challenge.message()?);
        Ok(ClientIdentityProof {
            version: challenge.version,
            public_key: encode_hex(&self.0.verifying_key().to_bytes()),
            challenge: challenge.nonce.clone(),
            signature: encode_hex(&signature.to_bytes()),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientIdentityProof {
    version: u8,
    public_key: String,
    challenge: String,
    signature: String,
}

impl ClientIdentityProof {
    /// Verifies proof of key possession against this connection's fresh challenge.
    pub fn verify(
        &self,
        expected: &AuthChallenge,
    ) -> Result<VerifiedClientIdentity, IdentityError> {
        if self.version != expected.version {
            return Err(IdentityError::UnsupportedVersion);
        }
        if self.challenge != expected.nonce {
            return Err(IdentityError::ChallengeMismatch);
        }
        let public_key =
            decode_hex::<32>(&self.public_key).ok_or(IdentityError::InvalidEncoding)?;
        let signature = decode_hex::<64>(&self.signature).ok_or(IdentityError::InvalidEncoding)?;
        let key = VerifyingKey::from_bytes(&public_key).map_err(|_| IdentityError::InvalidKey)?;
        key.verify_strict(&expected.message()?, &Signature::from_bytes(&signature))
            .map_err(|_| IdentityError::InvalidSignature)?;
        Ok(VerifiedClientIdentity(public_key))
    }
}

/// Public identity established by a valid signature; it grants no authority.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct VerifiedClientIdentity([u8; 32]);

impl VerifiedClientIdentity {
    #[must_use]
    pub const fn public_key(&self) -> &[u8; 32] {
        &self.0
    }

    /// Full SHA-256 fingerprint for native display; not a credential.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        encode_hex(&Sha256::digest(self.0))
    }
}

impl fmt::Debug for VerifiedClientIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("VerifiedClientIdentity")
            .field(&self.fingerprint())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityError {
    UnsupportedVersion,
    RandomUnavailable,
    InvalidEncoding,
    InvalidKey,
    ChallengeMismatch,
    InvalidSignature,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedVersion => "unsupported client authentication version",
            Self::RandomUnavailable => "client identity generation unavailable",
            Self::InvalidEncoding => "invalid client authentication encoding",
            Self::InvalidKey => "invalid client identity key",
            Self::ChallengeMismatch => "client authentication challenge mismatch",
            Self::InvalidSignature => "invalid client identity signature",
        })
    }
}

impl std::error::Error for IdentityError {}

fn decode_hex<const N: usize>(encoded: &str) -> Option<[u8; N]> {
    if encoded.len() != N * 2 || !encoded.is_ascii() {
        return None;
    }
    let mut decoded = [0; N];
    for (index, pair) in encoded.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_digit(pair[0])?;
        let low = hex_digit(pair[1])?;
        decoded[index] = (high << 4) | low;
    }
    Some(decoded)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_proof_is_bound_to_a_fresh_versioned_challenge() {
        let identity = ClientIdentity::from_hex_seed(&"07".repeat(32)).unwrap();
        let challenge = AuthChallenge::new([9; 32]);
        let proof = identity.proof(&challenge).unwrap();
        let verified = proof.verify(&challenge).unwrap();

        assert_eq!(verified.fingerprint().len(), 64);
        assert_eq!(
            VerifyingKey::from_bytes(verified.public_key())
                .unwrap()
                .to_bytes(),
            *verified.public_key()
        );
        assert_eq!(
            proof.verify(&AuthChallenge::new([8; 32])),
            Err(IdentityError::ChallengeMismatch)
        );
    }

    #[test]
    fn identity_proof_refuses_tampering_and_noncanonical_seeds() {
        let identity = ClientIdentity::from_hex_seed(&"07".repeat(32)).unwrap();
        let challenge = AuthChallenge::new([9; 32]);
        let mut proof = identity.proof(&challenge).unwrap();
        let replacement = if proof.signature.starts_with('0') {
            "1"
        } else {
            "0"
        };
        proof.signature.replace_range(0..1, replacement);
        assert_eq!(
            proof.verify(&challenge),
            Err(IdentityError::InvalidSignature)
        );
        assert!(matches!(
            ClientIdentity::from_hex_seed(&"0A".repeat(32)),
            Err(IdentityError::InvalidEncoding)
        ));
        assert!(matches!(
            ClientIdentity::from_hex_seed("07"),
            Err(IdentityError::InvalidEncoding)
        ));
    }

    #[test]
    fn generated_seed_is_parseable_and_has_the_required_length() {
        let mut seed = ClientIdentity::generate_seed_hex().unwrap();
        assert_eq!(seed.len(), 64);
        assert!(ClientIdentity::from_hex_seed(&seed).is_ok());
        zeroize::Zeroize::zeroize(&mut seed);
    }

    #[test]
    fn identity_wire_types_reject_unknown_fields() {
        let challenge = AuthChallenge::new([9; 32]);
        let json = serde_json::to_value(challenge).unwrap();
        let mut object = json.as_object().unwrap().clone();
        object.insert("approved".into(), serde_json::Value::Bool(true));
        assert!(serde_json::from_value::<AuthChallenge>(object.into()).is_err());
    }
}
