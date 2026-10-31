//! Platform-neutral, UI-free SFTP and sandbox file model for fesTerm mobile.
//!
//! * [`PaneFs`]: one trait over a [`LocalFs`] sandbox pane ("This iPhone")
//!   and an [`SftpClient`] remote pane: list (bounded, sortable, filterable),
//!   stat, mkdir, rename, delete.
//! * [`TransferManager`]: queued, cancellable, progress-reporting uploads and
//!   downloads with temp-file-then-atomic-rename commits and the
//!   Keep both / Skip / Replace conflict flow.
//! * [`path`]: the shared name/path sanitizer.
//!
//! Callers are async-free: pane calls block (run them on a worker thread) and
//! transfers report through polled events plus an optional wake callback.
//! The remote side wraps an already authenticated `russh-sftp` session; with
//! `festerm-ssh` use `connect_gui_sftp_session(..)` then
//! `SftpSession::into_transport()` and [`SftpClient::from_russh`].
//!
//! Trust boundaries: remote names and listings are untrusted (bounded and
//! sanitized, flagged via [`Entry::name_safe`]); local paths are confined to
//! the sandbox root, see [`LocalFs`] and [`SftpClient`] for the symlink policy.

mod cancel;
mod conflict;
mod error;
mod fs;
mod local;
mod model;
pub mod path;
mod remote;
mod transfer;

pub use cancel::CancelToken;
pub use conflict::{
    keep_both_name, likely_identical, Conflict, ConflictPolicy, Fingerprint, MTIME_TOLERANCE_SECS,
};
pub use error::{Result, SftpError};
pub use fs::{PaneFs, PaneKind, MAX_DELETE_DEPTH, MAX_DELETE_ENTRIES};
pub use local::LocalFs;
pub use model::{
    Entry, EntryKind, ListOptions, Listing, SortKey, SortOrder, SymlinkInfo, ABSOLUTE_MAX_SCAN,
    DEFAULT_MAX_ENTRIES, DEFAULT_MAX_SCAN,
};
pub use remote::{ClientConfig, SftpClient, MAX_SYMLINK_RESOLVES};
pub use transfer::{
    Direction, Progress, TransferConfig, TransferEvent, TransferId, TransferInfo, TransferManager,
    TransferRequest, TransferState,
};
