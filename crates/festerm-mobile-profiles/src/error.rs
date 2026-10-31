use std::fmt;

use festerm_secret_store::SecretStoreError;

/// A rejected field. It names the field and a fixed reason; it never echoes
/// the rejected value, which may be pasted secret material.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidationError {
    pub field: &'static str,
    pub reason: &'static str,
}

impl ValidationError {
    pub(crate) const fn new(field: &'static str, reason: &'static str) -> Self {
        Self { field, reason }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.reason)
    }
}

impl std::error::Error for ValidationError {}

/// Content-free failures of a document file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageError {
    /// The sandbox directory or file could not be read or written.
    Io,
    /// A document exceeds its size bound and was neither read nor written.
    TooLarge,
    /// The document was written by a newer app version. It is left untouched
    /// so a downgrade can never destroy data.
    UnsupportedVersion { found: u32 },
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io => formatter.write_str("profile storage could not be read or written"),
            Self::TooLarge => formatter.write_str("profile storage exceeds its size limit"),
            Self::UnsupportedVersion { found } => write!(
                formatter,
                "profile storage version {found} was written by a newer app"
            ),
        }
    }
}

impl std::error::Error for StorageError {}

/// Failures of the profile store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileError {
    Validation(ValidationError),
    Storage(StorageError),
    Secret(SecretStoreError),
    NotFound,
    DuplicateName,
    UnknownAccount,
    LimitReached,
    /// A reorder request was not a permutation of the stored profile ids.
    InvalidOrder,
}

impl fmt::Display for ProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
            Self::Secret(error) => write!(formatter, "secure storage failed: {error:?}"),
            Self::NotFound => formatter.write_str("profile not found"),
            Self::DuplicateName => formatter.write_str("a profile with this name already exists"),
            Self::UnknownAccount => formatter.write_str("the selected account is not signed in"),
            Self::LimitReached => formatter.write_str("the profile limit has been reached"),
            Self::InvalidOrder => formatter.write_str("the requested order is not a permutation"),
        }
    }
}

impl std::error::Error for ProfileError {}

impl From<ValidationError> for ProfileError {
    fn from(error: ValidationError) -> Self {
        Self::Validation(error)
    }
}

impl From<StorageError> for ProfileError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<SecretStoreError> for ProfileError {
    fn from(error: SecretStoreError) -> Self {
        Self::Secret(error)
    }
}
