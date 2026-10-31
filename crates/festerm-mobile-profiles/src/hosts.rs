//! Persistent SSH host-key trust (ADR 0020), kept apart from profiles so a
//! corrupt or replaced profile document can never alter trust.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use festerm_ssh::{is_sha256_fingerprint, HostIdentity};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::clock::{Clock, SystemClock};
use crate::document::{load_document, write_atomic, Loaded, Migration};
use crate::error::StorageError;

pub const KNOWN_HOSTS_FILE: &str = "known_hosts.json";
const CURRENT_VERSION: u32 = 1;
const MAX_FILE_BYTES: usize = 1024 * 1024;
pub const MAX_KNOWN_HOSTS: usize = 2000;
const MIGRATIONS: &[Migration] = &[];

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnownHost {
    pub host: String,
    pub port: u16,
    /// `SHA256:<base64>` of the server's host key.
    pub fingerprint: String,
    pub first_trusted_at: u64,
    pub last_seen_at: u64,
}

impl KnownHost {
    fn validate(&self) -> bool {
        HostIdentity::new(self.host.clone(), self.port).is_ok_and(|identity| {
            identity.host() == self.host && !self.host.chars().any(char::is_control)
        }) && valid_fingerprint(&self.fingerprint)
    }
}

fn valid_fingerprint(fingerprint: &str) -> bool {
    fingerprint.len() <= 128 && is_sha256_fingerprint(fingerprint)
}

/// What the store knows about a presented host key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostKeyStatus {
    /// First contact: ask the user to verify the fingerprint.
    Unknown,
    /// Matches the stored key; connect without prompting.
    Trusted,
}

/// Failures of host-key verification and trust persistence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostKeyError {
    /// The server presented a different key than the one trusted for this
    /// host and port. This is a hard refusal: it can only be resolved by the
    /// user explicitly forgetting the host with [`KnownHosts::forget`].
    Changed {
        host: String,
        port: u16,
        trusted_fingerprint: String,
        presented_fingerprint: String,
    },
    InvalidFingerprint,
    LimitReached,
    Storage(StorageError),
}

impl fmt::Display for HostKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Changed { host, port, .. } => write!(
                formatter,
                "the host key for {host}:{port} has changed; the connection was refused"
            ),
            Self::InvalidFingerprint => formatter.write_str("the host-key fingerprint is invalid"),
            Self::LimitReached => formatter.write_str("too many trusted hosts are stored"),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for HostKeyError {}

impl From<StorageError> for HostKeyError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

#[derive(Serialize, Deserialize)]
struct Document {
    version: u32,
    hosts: Vec<KnownHost>,
}

/// The persisted known-hosts list. Methods that change it save atomically
/// before returning and leave memory unchanged if the save fails.
pub struct KnownHosts {
    path: PathBuf,
    clock: Arc<dyn Clock>,
    hosts: Vec<KnownHost>,
    recovered: bool,
}

impl KnownHosts {
    /// Opens (creating on first save) `known_hosts.json` in `directory`.
    pub fn open(directory: &Path) -> Result<Self, StorageError> {
        Self::open_with_clock(directory, Arc::new(SystemClock))
    }

    pub fn open_with_clock(directory: &Path, clock: Arc<dyn Clock>) -> Result<Self, StorageError> {
        let path = directory.join(KNOWN_HOSTS_FILE);
        let mut recovered = false;
        let mut hosts = Vec::new();
        match load_document(&path, MAX_FILE_BYTES, CURRENT_VERSION, MIGRATIONS)? {
            Loaded::Missing => {}
            Loaded::Corrupt { .. } => recovered = true,
            Loaded::Document { value, .. } => {
                let entries = value
                    .get("hosts")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for entry in entries {
                    match serde_json::from_value::<KnownHost>(entry) {
                        Ok(host)
                            if host.validate()
                                && !hosts.iter().any(|other: &KnownHost| {
                                    (&other.host, other.port) == (&host.host, host.port)
                                })
                                && hosts.len() < MAX_KNOWN_HOSTS =>
                        {
                            hosts.push(host);
                        }
                        _ => recovered = true,
                    }
                }
            }
        }
        Ok(Self {
            path,
            clock,
            hosts,
            recovered,
        })
    }

    /// Whether unreadable data was discarded when opening. Trust for those
    /// hosts is simply asked again; nothing is silently trusted.
    #[must_use]
    pub fn recovered_from_corruption(&self) -> bool {
        self.recovered
    }

    #[must_use]
    pub fn hosts(&self) -> &[KnownHost] {
        &self.hosts
    }

    fn position(&self, identity: &HostIdentity) -> Option<usize> {
        self.hosts
            .iter()
            .position(|host| host.host == identity.host() && host.port == identity.port())
    }

    /// The trusted fingerprint to pass to
    /// `SshSessionOptions::with_known_host_fingerprint`.
    #[must_use]
    pub fn trusted_fingerprint(&self, identity: &HostIdentity) -> Option<&str> {
        self.position(identity)
            .map(|index| self.hosts[index].fingerprint.as_str())
    }

    /// Classifies a presented key. A different key for a known host is
    /// [`HostKeyError::Changed`] and must end the connection attempt.
    pub fn verify(
        &self,
        identity: &HostIdentity,
        presented_fingerprint: &str,
    ) -> Result<HostKeyStatus, HostKeyError> {
        if !valid_fingerprint(presented_fingerprint) {
            return Err(HostKeyError::InvalidFingerprint);
        }
        match self.position(identity).map(|index| &self.hosts[index]) {
            None => Ok(HostKeyStatus::Unknown),
            Some(known) if known.fingerprint == presented_fingerprint => Ok(HostKeyStatus::Trusted),
            Some(known) => Err(HostKeyError::Changed {
                host: known.host.clone(),
                port: known.port,
                trusted_fingerprint: known.fingerprint.clone(),
                presented_fingerprint: presented_fingerprint.to_owned(),
            }),
        }
    }

    /// Records the user's decision to trust `fingerprint` (first use), or
    /// refreshes `last_seen_at` for an already trusted key. It never replaces
    /// a different key: that returns [`HostKeyError::Changed`].
    pub fn trust(
        &mut self,
        identity: &HostIdentity,
        fingerprint: &str,
    ) -> Result<(), HostKeyError> {
        let status = self.verify(identity, fingerprint)?;
        let now = self.clock.now_unix();
        let mut hosts = self.hosts.clone();
        match status {
            HostKeyStatus::Trusted => {
                let index = self.position(identity).expect("trusted host exists");
                hosts[index].last_seen_at = now;
            }
            HostKeyStatus::Unknown => {
                if hosts.len() >= MAX_KNOWN_HOSTS {
                    return Err(HostKeyError::LimitReached);
                }
                hosts.push(KnownHost {
                    host: identity.host().to_owned(),
                    port: identity.port(),
                    fingerprint: fingerprint.to_owned(),
                    first_trusted_at: now,
                    last_seen_at: now,
                });
            }
        }
        self.commit(hosts)?;
        Ok(())
    }

    /// Explicitly revokes trust for a host, the only way past a changed key.
    /// Returns whether an entry existed.
    pub fn forget(&mut self, identity: &HostIdentity) -> Result<bool, HostKeyError> {
        let Some(index) = self.position(identity) else {
            return Ok(false);
        };
        let mut hosts = self.hosts.clone();
        hosts.remove(index);
        self.commit(hosts)?;
        Ok(true)
    }

    fn commit(&mut self, hosts: Vec<KnownHost>) -> Result<(), StorageError> {
        let document = Document {
            version: CURRENT_VERSION,
            hosts,
        };
        let bytes = serde_json::to_vec_pretty(&document).map_err(|_| StorageError::Io)?;
        write_atomic(&self.path, &bytes, MAX_FILE_BYTES)?;
        self.hosts = document.hosts;
        Ok(())
    }
}
