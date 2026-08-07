//! Native SSH transport policy and session foundations.
//!
//! The transport will implement [`festerm_session::Session`]. Terminal-core
//! mutation remains in the application, while this crate owns remote transport,
//! host trust, and reconnect policy.

use std::{fmt, time::Duration};

/// Canonical SSH destination identity used for trust decisions.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct HostIdentity {
    host: String,
    port: u16,
}

impl HostIdentity {
    pub fn new(host: impl Into<String>, port: u16) -> Result<Self, HostIdentityError> {
        let host = host.into();
        let host = host.trim();
        if host.is_empty() {
            return Err(HostIdentityError::EmptyHost);
        }
        if host.chars().any(char::is_whitespace) {
            return Err(HostIdentityError::Whitespace);
        }
        if port == 0 {
            return Err(HostIdentityError::ZeroPort);
        }
        Ok(Self {
            host: host.to_ascii_lowercase(),
            port,
        })
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub const fn port(&self) -> u16 {
        self.port
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostIdentityError {
    EmptyHost,
    Whitespace,
    ZeroPort,
}

impl fmt::Display for HostIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyHost => formatter.write_str("SSH host must not be empty"),
            Self::Whitespace => formatter.write_str("SSH host must not contain whitespace"),
            Self::ZeroPort => formatter.write_str("SSH port must not be zero"),
        }
    }
}

impl std::error::Error for HostIdentityError {}

/// Application response to a host-key verification prompt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostTrustDecision {
    Reject,
    AcceptOnce,
    AcceptAndPersist,
}

/// Bounded automatic reconnect behavior owned by the application.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReconnectPolicy {
    maximum_attempts: u8,
    initial_delay: Duration,
    maximum_delay: Duration,
}

impl ReconnectPolicy {
    pub fn new(
        maximum_attempts: u8,
        initial_delay: Duration,
        maximum_delay: Duration,
    ) -> Result<Self, ReconnectPolicyError> {
        if maximum_attempts == 0 {
            return Err(ReconnectPolicyError::ZeroAttempts);
        }
        if initial_delay.is_zero() {
            return Err(ReconnectPolicyError::ZeroInitialDelay);
        }
        if maximum_delay < initial_delay {
            return Err(ReconnectPolicyError::MaximumBeforeInitial);
        }
        Ok(Self {
            maximum_attempts,
            initial_delay,
            maximum_delay,
        })
    }

    pub const fn maximum_attempts(self) -> u8 {
        self.maximum_attempts
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconnectPolicyError {
    ZeroAttempts,
    ZeroInitialDelay,
    MaximumBeforeInitial,
}

impl fmt::Display for ReconnectPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroAttempts => formatter.write_str("reconnect requires at least one attempt"),
            Self::ZeroInitialDelay => formatter.write_str("reconnect delay must be nonzero"),
            Self::MaximumBeforeInitial => {
                formatter.write_str("maximum reconnect delay must not precede initial delay")
            }
        }
    }
}

impl std::error::Error for ReconnectPolicyError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_identity_normalizes_and_rejects_ambiguous_input() {
        let host = HostIdentity::new(" Example.COM ", 22).unwrap();
        assert_eq!(host.host(), "example.com");
        assert_eq!(host.port(), 22);
        assert_eq!(
            HostIdentity::new(" ", 22),
            Err(HostIdentityError::EmptyHost)
        );
        assert_eq!(
            HostIdentity::new("example host", 22),
            Err(HostIdentityError::Whitespace)
        );
        assert_eq!(
            HostIdentity::new("example.com", 0),
            Err(HostIdentityError::ZeroPort)
        );
    }

    #[test]
    fn reconnect_policy_requires_finite_ordered_bounds() {
        assert_eq!(
            ReconnectPolicy::new(0, Duration::from_secs(1), Duration::from_secs(2)),
            Err(ReconnectPolicyError::ZeroAttempts)
        );
        assert_eq!(
            ReconnectPolicy::new(1, Duration::ZERO, Duration::from_secs(2)),
            Err(ReconnectPolicyError::ZeroInitialDelay)
        );
        assert_eq!(
            ReconnectPolicy::new(1, Duration::from_secs(2), Duration::from_secs(1)),
            Err(ReconnectPolicyError::MaximumBeforeInitial)
        );
    }
}
