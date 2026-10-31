use std::fmt;

/// Failure classes the UI can present without parsing text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SftpError {
    /// The path or name failed sanitization (traversal, NUL, control or bidi
    /// characters, over-long components, escapes the local root).
    InvalidPath(String),
    NotFound(String),
    PermissionDenied(String),
    AlreadyExists(String),
    NotADirectory(String),
    IsADirectory(String),
    DirectoryNotEmpty(String),
    /// A symlink was refused by policy (see crate docs).
    SymlinkRefused(String),
    /// The transport is gone or stopped answering mid-operation.
    Disconnected(String),
    Timeout(String),
    Cancelled,
    /// A configured resource bound was hit.
    LimitExceeded(String),
    Unsupported(String),
    /// Any other server-reported failure.
    Remote(String),
    /// Any other local I/O failure.
    Io(String),
}

impl fmt::Display for SftpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath(m) => write!(f, "invalid path: {m}"),
            Self::NotFound(m) => write!(f, "not found: {m}"),
            Self::PermissionDenied(m) => write!(f, "permission denied: {m}"),
            Self::AlreadyExists(m) => write!(f, "already exists: {m}"),
            Self::NotADirectory(m) => write!(f, "not a directory: {m}"),
            Self::IsADirectory(m) => write!(f, "is a directory: {m}"),
            Self::DirectoryNotEmpty(m) => write!(f, "directory not empty: {m}"),
            Self::SymlinkRefused(m) => write!(f, "symlink refused: {m}"),
            Self::Disconnected(m) => write!(f, "disconnected: {m}"),
            Self::Timeout(m) => write!(f, "timed out: {m}"),
            Self::Cancelled => f.write_str("cancelled"),
            Self::LimitExceeded(m) => write!(f, "limit exceeded: {m}"),
            Self::Unsupported(m) => write!(f, "unsupported: {m}"),
            Self::Remote(m) => write!(f, "server error: {m}"),
            Self::Io(m) => write!(f, "i/o error: {m}"),
        }
    }
}

impl std::error::Error for SftpError {}

pub type Result<T> = std::result::Result<T, SftpError>;

impl SftpError {
    /// True when the connection itself is unusable and the UI should offer
    /// reconnect rather than retry.
    pub fn is_connection_lost(&self) -> bool {
        matches!(self, Self::Disconnected(_) | Self::Timeout(_))
    }

    pub(crate) fn from_io(error: &std::io::Error, context: &str) -> Self {
        use std::io::ErrorKind::*;
        let message = format!("{context}: {error}");
        match error.kind() {
            NotFound => Self::NotFound(message),
            PermissionDenied => Self::PermissionDenied(message),
            AlreadyExists => Self::AlreadyExists(message),
            NotADirectory => Self::NotADirectory(message),
            IsADirectory => Self::IsADirectory(message),
            DirectoryNotEmpty => Self::DirectoryNotEmpty(message),
            _ => Self::Io(message),
        }
    }

    pub(crate) fn from_remote(error: &russh_sftp::client::error::Error, context: &str) -> Self {
        use russh_sftp::client::error::Error as E;
        use russh_sftp::protocol::StatusCode as S;
        let message = format!("{context}: {error}");
        match error {
            E::Status(status) => match status.status_code {
                S::NoSuchFile => Self::NotFound(message),
                S::PermissionDenied => Self::PermissionDenied(message),
                S::NoConnection | S::ConnectionLost => Self::Disconnected(message),
                S::OpUnsupported => Self::Unsupported(message),
                _ => Self::Remote(message),
            },
            E::Timeout => Self::Timeout(message),
            E::Limited(_) => Self::LimitExceeded(message),
            // Channel closure surfaces as I/O, dropped senders or unexpected
            // behavior; all mean the transport can no longer be trusted.
            _ => Self::Disconnected(message),
        }
    }
}
