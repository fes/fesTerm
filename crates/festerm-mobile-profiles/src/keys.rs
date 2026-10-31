//! SSH private-key import and generation for the mobile app.
//!
//! Any key `russh` can decode (OpenSSH, PKCS#8 and PuTTY formats; Ed25519 and
//! ECDSA P-256/384/521) is normalized to an unencrypted OpenSSH document and
//! stored as one secret-store item. The passphrase is used only to decrypt and
//! is never retained: the Keychain item (device-only, readable when unlocked)
//! is the protection boundary, and keeping a passphrase next to the key it
//! protects would add no confidentiality.

use std::fmt;

use festerm_secret_store::{SecretBytes, SecretStore, SecretStoreError};
use russh::keys::{ssh_key::LineEnding, Algorithm, HashAlg, PrivateKey};
use zeroize::Zeroizing;

use crate::model::{KeyInfo, SecretRef};

/// Pasted or imported key text larger than this is refused unread.
pub const MAX_KEY_TEXT_BYTES: usize = 32 * 1024;
const MAX_COMMENT_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyImportError {
    Empty,
    TooLarge,
    /// The user pasted a public key where a private key is needed.
    LooksLikePublicKey,
    /// Not a private key in a format or algorithm this app supports.
    Unsupported,
    /// The key is encrypted and no passphrase was supplied.
    PassphraseRequired,
    /// The passphrase did not decrypt the key, or the key is damaged.
    WrongPassphraseOrInvalid,
    /// The system random generator or encoder failed.
    GenerationFailed,
}

impl fmt::Display for KeyImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "no key text was provided",
            Self::TooLarge => "the key text is too large",
            Self::LooksLikePublicKey => "this is a public key; a private key is required",
            Self::Unsupported => "this is not a supported private key format",
            Self::PassphraseRequired => "this key is encrypted; enter its passphrase",
            Self::WrongPassphraseOrInvalid => "the passphrase is wrong or the key is damaged",
            Self::GenerationFailed => "a key could not be generated",
        })
    }
}

impl std::error::Error for KeyImportError {}

/// A decoded private key ready to store. It has no `Clone`, and `Debug`
/// is redacted; the secret text is wiped on drop.
pub struct ParsedPrivateKey {
    openssh: Zeroizing<String>,
    info: KeyInfo,
}

impl ParsedPrivateKey {
    /// Public facts for display and `authorized_keys`.
    #[must_use]
    pub fn info(&self) -> &KeyInfo {
        &self.info
    }

    fn from_key(key: &PrivateKey) -> Result<Self, KeyImportError> {
        let openssh = key
            .to_openssh(LineEnding::LF)
            .map_err(|_| KeyImportError::Unsupported)?;
        let mut public = key.public_key().clone();
        public.set_comment("");
        let comment = key.comment().to_string();
        Ok(Self {
            openssh: Zeroizing::new(openssh.to_string()),
            info: KeyInfo {
                algorithm: key.algorithm().to_string(),
                fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
                comment: comment
                    .chars()
                    .filter(|character| !character.is_control())
                    .take(MAX_COMMENT_BYTES)
                    .collect(),
                public_key: public
                    .to_openssh()
                    .map_err(|_| KeyImportError::Unsupported)?,
            },
        })
    }
}

impl fmt::Debug for ParsedPrivateKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ParsedPrivateKey([REDACTED])")
    }
}

/// Decodes key text from Files or the clipboard.
///
/// `passphrase` is needed only for encrypted keys and is ignored otherwise.
pub fn parse_private_key(
    text: &str,
    passphrase: Option<&str>,
) -> Result<ParsedPrivateKey, KeyImportError> {
    if text.len() > MAX_KEY_TEXT_BYTES {
        return Err(KeyImportError::TooLarge);
    }
    let normalized = Zeroizing::new(text.trim().replace("\r\n", "\n"));
    if normalized.is_empty() {
        return Err(KeyImportError::Empty);
    }
    let key = match russh::keys::decode_secret_key(&normalized, None) {
        Ok(key) => key,
        Err(russh::keys::Error::KeyIsEncrypted) => {
            let passphrase = passphrase
                .filter(|passphrase| !passphrase.is_empty())
                .ok_or(KeyImportError::PassphraseRequired)?;
            russh::keys::decode_secret_key(&normalized, Some(passphrase))
                .map_err(|_| KeyImportError::WrongPassphraseOrInvalid)?
        }
        Err(_) if looks_like_public_key(&normalized) => {
            return Err(KeyImportError::LooksLikePublicKey)
        }
        Err(_) => return Err(KeyImportError::Unsupported),
    };
    ParsedPrivateKey::from_key(&key)
}

fn looks_like_public_key(text: &str) -> bool {
    ["ssh-", "ecdsa-", "sk-"]
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

/// Generates a new Ed25519 key; `comment` (for example `iPhone`) is shown in
/// the key list and embedded in the public key.
pub fn generate_ed25519(comment: &str) -> Result<ParsedPrivateKey, KeyImportError> {
    let mut random = russh::keys::key::safe_rng();
    let mut key = PrivateKey::random(&mut random, Algorithm::Ed25519)
        .map_err(|_| KeyImportError::GenerationFailed)?;
    key.set_comment(
        comment
            .chars()
            .filter(|character| !character.is_control())
            .take(MAX_COMMENT_BYTES)
            .collect::<String>(),
    );
    ParsedPrivateKey::from_key(&key)
}

/// Stores `key` in `store` in the format `festerm-ssh` resolves for stored
/// private-key authentication, returning its reference and public facts.
pub fn store_private_key(
    store: &dyn SecretStore,
    key: ParsedPrivateKey,
) -> Result<(SecretRef, KeyInfo), SecretStoreError> {
    let secret: SecretBytes = festerm_ssh::encode_stored_private_key(&key.openssh, None);
    let reference = store.put(&secret)?;
    Ok((SecretRef::from_reference(&reference), key.info.clone()))
}
