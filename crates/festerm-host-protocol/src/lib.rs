//! Shared HOST contracts, not a network service or an authorization credential.
//!
//! Use [`decode`] and [`encode`] at the untrusted wire boundary. A successful
//! parse establishes syntax and bounds only, never principal identity or access.

mod identity;
mod inventory;
mod resources;
mod wire;

pub use identity::*;
pub use inventory::*;
pub use resources::*;
pub use wire::*;

use std::{fmt, io};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContractError {
    Malformed,
    UnsupportedVersion,
    Limit,
    Denied,
    Stale,
    Busy,
    Exhausted,
}

impl fmt::Display for ContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Malformed => "malformed host contract",
            Self::UnsupportedVersion => "unsupported host protocol",
            Self::Limit => "host contract limit exceeded",
            Self::Denied => "host operation denied",
            Self::Stale => "stale host operation",
            Self::Busy => "host budget exhausted",
            Self::Exhausted => "host epoch exhausted",
        })
    }
}

impl std::error::Error for ContractError {}

impl From<ContractError> for io::Error {
    fn from(error: ContractError) -> Self {
        Self::new(io::ErrorKind::InvalidData, error)
    }
}

pub trait Validate {
    fn validate(&self) -> Result<(), ContractError>;

    /// Versioned envelopes inspect bounded headers before decoding a body
    /// whose schema may belong to a newer or legacy protocol.
    fn validate_wire_header(_bytes: &[u8]) -> Result<(), ContractError>
    where
        Self: Sized,
    {
        Ok(())
    }
}
