use crate::ContractError;
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

macro_rules! identity {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn new() -> Self {
                Self(uuid::Uuid::new_v4().hyphenated().to_string())
            }
            pub fn as_str(&self) -> &str { &self.0 }
        }
        impl Default for $name { fn default() -> Self { Self::new() } }
        impl TryFrom<String> for $name {
            type Error = ContractError;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                let id = uuid::Uuid::parse_str(&value).map_err(|_| ContractError::Malformed)?;
                if id.get_version_num() != 4
                    || id.get_variant() != uuid::Variant::RFC4122
                    || value != id.hyphenated().to_string()
                {
                    return Err(ContractError::Malformed);
                }
                Ok(Self(value))
            }
        }
        impl FromStr for $name {
            type Err = ContractError;
            fn from_str(value: &str) -> Result<Self, Self::Err> { value.to_owned().try_into() }
        }
        impl From<$name> for String { fn from(value: $name) -> Self { value.0 } }
        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    )+};
}

identity!(
    HostId,
    LogicalSessionId,
    OperationId,
    PrincipalId,
    AttachmentId,
    LeaseId,
    ConnectionId,
);

/// Existing sessiond generation, not a PID, label, or reusable logical ID.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct GenerationId(u128);

impl GenerationId {
    pub fn new(value: u128) -> Result<Self, ContractError> {
        if value == 0 {
            return Err(ContractError::Malformed);
        }
        Ok(Self(value))
    }
    pub const fn get(self) -> u128 {
        self.0
    }
}

impl TryFrom<String> for GenerationId {
    type Error = ContractError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let generation = Self::new(value.parse().map_err(|_| ContractError::Malformed)?)?;
        if value != generation.0.to_string() {
            return Err(ContractError::Malformed);
        }
        Ok(generation)
    }
}

impl From<GenerationId> for String {
    fn from(value: GenerationId) -> Self {
        value.0.to_string()
    }
}

/// Monotonic, journaled host/grant/lease revision; zero is never admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct Epoch(u64);

impl Epoch {
    pub fn new(value: u64) -> Result<Self, ContractError> {
        if value == 0 {
            return Err(ContractError::Malformed);
        }
        Ok(Self(value))
    }
    pub const fn get(self) -> u64 {
        self.0
    }
    pub fn next(self) -> Result<Self, ContractError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(ContractError::Exhausted)
    }
}

impl TryFrom<u64> for Epoch {
    type Error = ContractError;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Epoch> for u64 {
    fn from(value: Epoch) -> Self {
        value.0
    }
}
