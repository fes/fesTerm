use std::fmt;

use festerm_tunnel_bridge::mobile::{AttachError, Displacer, EndReason};
use festerm_tunnel_client::Error as ClientError;

/// Every failure the UI must distinguish. Messages are safe to show: they never
/// contain tokens, codes or tunnel connect credentials.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TunnelError {
    /// No usable stored credential; run the device sign-in again.
    SignInRequired,
    NotConfigured,
    Cancelled,
    /// The 10-15 minute device code or the credential expired.
    Expired,
    Denied,
    Network,
    RateLimited,
    /// The signed-in GitHub account is no longer the one the profile is bound to.
    AccountChanged,
    SecureStorage,
    TunnelNotFound,
    /// The tunnel exists but hosts no fesTerm bridge, or hosts several.
    NoBridge,
    Ambiguous,
    /// First connection to this host: the user must approve its fingerprint.
    HostNotTrusted {
        fingerprint: String,
    },
    /// The host key or identity differs from the approved one.
    HostChanged {
        approved: String,
        presented: String,
    },
    SessionNotFound,
    /// Someone holds the session and this profile does not allow takeover (or
    /// the host forbids it).
    SessionInUse {
        by: Option<Displacer>,
    },
    /// Another client took the session over; reconnecting is a user decision.
    Displaced(Displacer),
    ConnectionLost,
    SessionExited,
    Protocol,
    Timeout,
    Profile(String),
}

impl fmt::Display for TunnelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SignInRequired => f.write_str("Sign in with GitHub to continue."),
            Self::NotConfigured => f.write_str("GitHub sign-in is not configured in this build."),
            Self::Cancelled => f.write_str("Cancelled."),
            Self::Expired => f.write_str("Sign-in expired. Try again."),
            Self::Denied => f.write_str("Access denied. Check the account and tunnel."),
            Self::Network => f.write_str("Connection failed. Try again."),
            Self::RateLimited => f.write_str("Rate limit reached. Try later."),
            Self::AccountChanged => f.write_str("The GitHub account changed. Sign in again."),
            Self::SecureStorage => f.write_str("Secure storage unavailable."),
            Self::TunnelNotFound => f.write_str("Tunnel not found. Check the name and account."),
            Self::NoBridge => f.write_str("No fesTerm bridge is available for this tunnel."),
            Self::Ambiguous => f.write_str("This tunnel has multiple hosts or ports."),
            Self::HostNotTrusted { .. } => f.write_str("Approve this host before connecting."),
            Self::HostChanged { .. } => {
                f.write_str("The host identity changed. Verify it before connecting.")
            }
            Self::SessionNotFound => f.write_str("The remote session is no longer available."),
            Self::SessionInUse { .. } => {
                f.write_str("The session is attached elsewhere and takeover is off.")
            }
            Self::Displaced(by) => EndReason::Displaced(*by).fmt(f),
            Self::ConnectionLost => f.write_str("Connection lost."),
            Self::SessionExited => f.write_str("The remote shell exited."),
            Self::Protocol => f.write_str("Unexpected host response."),
            Self::Timeout => f.write_str("The operation timed out."),
            Self::Profile(reason) => write!(f, "Profile problem: {reason}"),
        }
    }
}
impl std::error::Error for TunnelError {}

impl TunnelError {
    /// The end of a live attachment, as the UI should present it.
    #[must_use]
    pub fn from_end(reason: EndReason) -> Option<Self> {
        match reason {
            EndReason::Displaced(by) => Some(Self::Displaced(by)),
            EndReason::Exited => Some(Self::SessionExited),
            EndReason::ConnectionLost => Some(Self::ConnectionLost),
            EndReason::Detached => None,
        }
    }

    /// Whether signing in again is the remedy.
    #[must_use]
    pub fn needs_sign_in(&self) -> bool {
        matches!(
            self,
            Self::SignInRequired | Self::Expired | Self::AccountChanged
        )
    }
}

impl From<ClientError> for TunnelError {
    fn from(error: ClientError) -> Self {
        match error {
            ClientError::Registration | ClientError::IdentityChanged => Self::NotConfigured,
            ClientError::Cancelled => Self::Cancelled,
            ClientError::Expired => Self::Expired,
            ClientError::Denied => Self::Denied,
            ClientError::Network => Self::Network,
            ClientError::Response => Self::Protocol,
            ClientError::AccountChanged => Self::AccountChanged,
            ClientError::TunnelNotFound => Self::TunnelNotFound,
            ClientError::Tunnel => Self::NoBridge,
            ClientError::Ambiguous => Self::Ambiguous,
            ClientError::Store => Self::SecureStorage,
            ClientError::Busy => Self::Network,
            ClientError::RateLimited => Self::RateLimited,
        }
    }
}

impl From<AttachError> for TunnelError {
    fn from(error: AttachError) -> Self {
        match error {
            AttachError::Refused => Self::SessionInUse { by: None },
            AttachError::Protocol => Self::Protocol,
            AttachError::Connection => Self::Network,
            AttachError::Cancelled => Self::Cancelled,
        }
    }
}

impl From<festerm_mobile_profiles::ProfileError> for TunnelError {
    fn from(error: festerm_mobile_profiles::ProfileError) -> Self {
        Self::Profile(error.to_string())
    }
}
