use crate::{
    error::Result,
    model::{Entry, ListOptions, Listing},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneKind {
    /// This device's sandbox (paths are relative to the sandbox root).
    Local,
    /// A remote SFTP server (paths are server paths).
    Remote,
}

/// One browsable pane. Both the local sandbox and the remote server implement
/// it so the UI can drive stacked panes with one code path.
///
/// Calls block the calling thread (remote calls wait for the network, bounded
/// by timeouts); call them from a worker thread, never the UI frame.
pub trait PaneFs: Send + Sync {
    fn kind(&self) -> PaneKind;
    /// Directory shown when the pane opens ("" for the local root).
    fn home(&self) -> String;
    fn list(&self, path: &str, options: &ListOptions) -> Result<Listing>;
    /// Metadata of the entry itself (symlinks are not followed, but a
    /// resolvable link reports its target kind and size).
    fn stat(&self, path: &str) -> Result<Entry>;
    /// Creates one directory; the parent must exist; fails if it exists.
    fn mkdir(&self, path: &str) -> Result<()>;
    /// Renames or moves within the pane; never replaces an existing target.
    fn rename(&self, from: &str, to: &str) -> Result<()>;
    /// Deletes a file, a link (never its target) or a directory. A
    /// non-empty directory needs `recursive`. The caller owns confirmation.
    fn delete(&self, path: &str, recursive: bool) -> Result<()>;
    fn join(&self, directory: &str, name: &str) -> Result<String>;
    fn parent(&self, path: &str) -> Option<String>;
}

/// Bounds for recursive deletion so a hostile or huge tree cannot run away.
pub const MAX_DELETE_ENTRIES: usize = 50_000;
pub const MAX_DELETE_DEPTH: usize = 32;
