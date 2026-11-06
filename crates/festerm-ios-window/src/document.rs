//! Native Files authority and bounded serialization; no editor or speech state.

use std::path::PathBuf;

pub const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
pub const MAX_NATIVE_DOCUMENTS: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentFileError {
    Unsupported,
    Busy,
    Cancelled,
    Unavailable,
    ReadOnly,
    TooLarge,
    Changed,
    NotAFile,
    Io,
}

impl DocumentFileError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Unsupported => "Browse in Files is available only in the mobile app.",
            Self::Busy => "Another file operation is active. Finish or cancel it and try again.",
            Self::Cancelled => "The file operation was cancelled.",
            Self::Unavailable => "Access to this file is unavailable. Reopen it through Files.",
            Self::ReadOnly => "This file is read-only. Save an editable local copy.",
            Self::TooLarge => "This file exceeds the one MiB mobile document limit.",
            Self::Changed => "The file changed before saving. Refresh and resolve the conflict.",
            Self::NotAFile => "Choose a regular text file, not a folder or file package.",
            Self::Io => "The file operation failed. Your editor content has been kept.",
        }
    }
}

/// Transient I/O payload. Its path is display/identity metadata, not authority.
pub struct DocumentFileSnapshot {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub version: [u8; 32],
    pub writable: bool,
}

pub enum DocumentFileEvent {
    Loaded(DocumentFileSnapshot),
    Saved {
        operation: u64,
        version: [u8; 32],
    },
    SaveFailed {
        operation: u64,
        error: DocumentFileError,
    },
    Unavailable(DocumentFileError),
}

#[cfg(target_os = "ios")]
#[path = "document_ios.rs"]
mod native;
#[cfg(all(
    target_os = "ios",
    target_abi = "sim",
    feature = "native-document-smoke"
))]
pub use native::DocumentSmoke;
#[cfg(target_os = "ios")]
pub use native::{DocumentFile, DocumentPicker};

#[cfg(any(target_os = "android", test))]
#[path = "document_android.rs"]
mod android;
#[cfg(target_os = "android")]
pub use android::{DocumentFile, DocumentPicker};

#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub struct DocumentPicker;

#[cfg(not(any(target_os = "ios", target_os = "android")))]
impl DocumentPicker {
    pub fn open(&self) -> Result<(), DocumentFileError> {
        Err(DocumentFileError::Unsupported)
    }

    pub fn take_result(&self) -> Option<Result<DocumentFile, DocumentFileError>> {
        None
    }
}

#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub struct DocumentFile;

#[cfg(not(any(target_os = "ios", target_os = "android")))]
impl DocumentFile {
    pub fn open(&self) -> Result<(), DocumentFileError> {
        Err(DocumentFileError::Unsupported)
    }

    pub fn refresh(&self) -> Result<(), DocumentFileError> {
        Err(DocumentFileError::Unsupported)
    }

    pub fn save(
        &self,
        _operation: u64,
        _expected: [u8; 32],
        _bytes: Vec<u8>,
    ) -> Result<(), DocumentFileError> {
        Err(DocumentFileError::Unsupported)
    }

    pub fn take_events(&self) -> Vec<DocumentFileEvent> {
        Vec::new()
    }
}
