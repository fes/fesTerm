#![forbid(unsafe_code)]

//! Persistent profiles, host-key trust and SSH key management for the fesTerm
//! iOS app, independent of any UI.
//!
//! * [`ProfileStore`] keeps secret-free, versioned JSON profile documents
//!   (SSH, Dev Tunnel, SFTP) and Dev Tunnel accounts in a directory the host
//!   app provides. Passwords, private keys and tunnel tokens live only in a
//!   [`festerm_secret_store::SecretStore`] (the iOS Keychain in production),
//!   referenced by opaque per-profile ids and deleted with the profile.
//! * [`KnownHosts`] persists SSH host-key trust separately; a changed key is
//!   the typed hard refusal [`HostKeyError::Changed`].
//! * [`keys`] imports (OpenSSH/PKCS#8/PuTTY, optional passphrase) and
//!   generates (Ed25519) SSH keys.
//! * [`PublicProfileImport`] validates the `festerm-profile.json` printed by
//!   the one-line Dev Tunnel installer.
//!
//! This crate deliberately does not depend on `festerm-config`, which pulls in
//! desktop PTY code that the iOS graph forbids (ADR 0054); the Dev Tunnel
//! shapes and validation are mirrored here instead.

mod clock;
mod document;
mod error;
mod hosts;
pub mod keys;
mod model;
mod store;
mod tunnel_import;

pub use clock::{Clock, SystemClock};
pub use error::{ProfileError, StorageError, ValidationError};
pub use hosts::{
    HostKeyError, HostKeyStatus, KnownHost, KnownHosts, KNOWN_HOSTS_FILE, MAX_KNOWN_HOSTS,
};
pub use keys::{KeyImportError, ParsedPrivateKey};
pub use model::{
    AccountProvider, DevTunnelProfile, KeyInfo, Profile, ProfileId, ProfileKind, Recovery,
    RemoteSession, SecretRef, SftpProfile, SshAuth, SshProfile, TrustedTunnelHost, TunnelAccount,
    TunnelSource, DEFAULT_SSH_PORT, MAX_NAME_CHARS,
};
pub use store::{
    CredentialStatus, LoadReport, ProfileStore, CURRENT_VERSION, MAX_ACCOUNTS, MAX_FILE_BYTES,
    MAX_PROFILES, PROFILES_FILE,
};
pub use tunnel_import::{
    DestinationEndpoint, DestinationTarget, PathStyle, PublicDestination, PublicProfileImport,
    MAX_IMPORT_BYTES,
};
