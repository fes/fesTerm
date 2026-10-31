//! Public Dev Tunnel setup metadata printed by the one-line installer.
//!
//! The shapes mirror `festerm-tunnel-bridge`'s `Destination` (ADR 0053) so a
//! stored destination deserializes there unchanged, without pulling the
//! desktop bridge into the iOS graph. Nothing here authenticates or does I/O.

use serde::{Deserialize, Serialize};

use crate::error::ValidationError;

/// The installer's JSON is a few KiB; this bounds untrusted pasted text.
pub const MAX_IMPORT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_TRUSTED_HOST_FIELD: usize = 4096;
const RELAY_HOST_SUFFIX: &str = ".rel.tunnels.api.visualstudio.com";

pub(crate) fn is_identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

/// Dev Tunnels names: lowercase alphanumerics and hyphens, with at most one
/// dot separating tunnel and cluster.
pub(crate) fn validate_tunnel_name(name: &str) -> Result<(), ValidationError> {
    let invalid = name.is_empty()
        || name.len() > 201
        || !name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-.".contains(&byte)
        })
        || name.split('.').any(|part| {
            part.is_empty() || part.len() > 100 || part.starts_with('-') || part.ends_with('-')
        })
        || name.matches('.').count() > 1;
    if invalid {
        return Err(ValidationError::new("tunnel_name", "is invalid"));
    }
    Ok(())
}

fn valid_relay_uri(value: &str) -> bool {
    if value.len() > 4096 || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        return false;
    }
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    url.scheme() == "wss"
        && url
            .host_str()
            .is_some_and(|host| host.ends_with(RELAY_HOST_SUFFIX))
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none_or(|port| port == 443)
        && url.fragment().is_none()
        && url.query().is_none()
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathStyle {
    #[default]
    Posix,
    Windows,
}

/// The remote daemon session the destination targets.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationTarget {
    pub name: String,
    pub pid: u32,
    /// A decimal string, avoiding JSON `u128` interoperability problems.
    pub generation: String,
}

/// Relay binding data only. Unknown fields (command or URL templates the SDK
/// would silently accept) are refused.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DestinationEndpoint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub connection_mode: String,
    pub host_id: String,
    pub host_public_keys: Vec<String>,
    pub client_relay_uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_relay_uri: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicDestination {
    #[serde(default)]
    pub file_read: bool,
    #[serde(default)]
    pub path_style: PathStyle,
    pub version: u16,
    pub tunnel_id: String,
    pub cluster: String,
    pub port: u16,
    pub target: DestinationTarget,
    pub endpoint: DestinationEndpoint,
}

impl PublicDestination {
    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        let endpoint = &self.endpoint;
        let generation_ok = self.target.generation.len() <= 39
            && self
                .target
                .generation
                .bytes()
                .all(|byte| byte.is_ascii_digit())
            && self
                .target
                .generation
                .parse::<u128>()
                .is_ok_and(|value| value > 0);
        let valid = self.version == 1
            && is_identifier(&self.tunnel_id, 128)
            && is_identifier(&self.cluster, 32)
            && self.port != 0
            && is_identifier(&self.target.name, 128)
            && self.target.pid != 0
            && generation_ok
            && endpoint.connection_mode == "TunnelRelay"
            && is_identifier(&endpoint.host_id, 128)
            && endpoint
                .id
                .as_deref()
                .is_none_or(|id| is_identifier(id, 200))
            && endpoint.host_public_keys.len() == 1
            && valid_relay_uri(&endpoint.client_relay_uri)
            && endpoint
                .host_relay_uri
                .as_deref()
                .is_none_or(valid_relay_uri);
        if !valid {
            return Err(ValidationError::new("destination", "is invalid"));
        }
        self.host_fingerprint().map(|_| ())
    }

    /// The `SHA256:` fingerprint of the pinned tunnel-host key, for the user
    /// to compare with the installer's "Verify host fingerprint" line.
    pub fn host_fingerprint(&self) -> Result<String, ValidationError> {
        let invalid = || ValidationError::new("destination.host_public_key", "is invalid");
        let key = self.endpoint.host_public_keys.first().ok_or_else(invalid)?;
        if key.len() > MAX_TRUSTED_HOST_FIELD {
            return Err(invalid());
        }
        let key = russh::keys::parse_public_key_base64(key).map_err(|_| invalid())?;
        Ok(key.fingerprint(russh::keys::HashAlg::Sha256).to_string())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    festerm_profile_version: u16,
    name: String,
    destination: PublicDestination,
}

/// A validated, not yet saved `festerm-profile.json`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicProfileImport {
    name: String,
    destination: PublicDestination,
    fingerprint: String,
}

impl PublicProfileImport {
    /// Parses pasted or Files-picked installer output. Only version 1 public
    /// metadata is accepted; unknown fields and oversized input are refused.
    pub fn parse(text: &str) -> Result<Self, ValidationError> {
        let invalid = || ValidationError::new("import", "is not valid public setup JSON");
        if text.len() > MAX_IMPORT_BYTES {
            return Err(invalid());
        }
        let envelope: Envelope = serde_json::from_str(text).map_err(|_| invalid())?;
        if envelope.festerm_profile_version != 1 {
            return Err(invalid());
        }
        let mut name = envelope.name;
        crate::model::validate_name(&mut name)?;
        envelope.destination.validate()?;
        let fingerprint = envelope.destination.host_fingerprint()?;
        Ok(Self {
            name,
            destination: envelope.destination,
            fingerprint,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn destination(&self) -> &PublicDestination {
        &self.destination
    }

    #[must_use]
    pub fn host_fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub(crate) fn into_destination(self) -> PublicDestination {
        self.destination
    }
}
