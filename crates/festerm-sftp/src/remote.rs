//! Remote pane over an authenticated `russh-sftp` session.
//!
//! The client owns a small private tokio runtime so callers never touch
//! async: [`SftpClient`] methods block the calling thread (bounded by
//! timeouts) and transfers run in the background through
//! [`crate::TransferManager`]. Nothing here allocates unbounded memory from a
//! server-controlled count: listings stop at `max_scan`, symlink resolution
//! is capped, recursive deletion is depth/count bounded, every request has a
//! timeout.
//!
//! Symlink policy (remote): links are never traversed implicitly by this
//! crate. Listings report the link kind plus, when `stat` resolves it, the
//! target kind/size; `stat` of a link resolves it; downloads follow a link to
//! a regular file (the server decides what that file is; the *local* name
//! always comes from the link's own validated name); delete/rename act on the
//! link, never its target; recursive delete unlinks links without descending.

use crate::{
    cancel::CancelToken,
    error::{Result, SftpError},
    fs::{PaneFs, PaneKind, MAX_DELETE_DEPTH, MAX_DELETE_ENTRIES},
    model::{finish_listing, Entry, EntryKind, ListOptions, Listing, SymlinkInfo},
    path,
};
use russh_sftp::{
    client::{error::Error as RawError, rawsession::SftpResult, RawSftpSession, SftpSession},
    protocol::{FileAttributes, Packet, StatusCode},
};
use std::{
    any::Any,
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::Duration,
};

/// Symlinks resolved per listing; further links show as unresolved.
pub const MAX_SYMLINK_RESOLVES: usize = 128;

#[derive(Clone, Debug)]
pub struct ClientConfig {
    /// Per-SFTP-request timeout (a stalled server surfaces as `Timeout`).
    pub request_timeout: Duration,
    /// Upper bound for one blocking pane call (list/stat/mkdir/...), end to end.
    pub operation_timeout: Duration,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(30),
            operation_timeout: Duration::from_secs(120),
        }
    }
}

/// Async core shared (by `Arc`) with transfer tasks. It deliberately does not
/// own the runtime so a task dropping the last reference cannot tear the
/// runtime down from inside itself.
pub(crate) struct Remote {
    pub(crate) session: SftpSession,
    pub(crate) closed: AtomicBool,
    pub(crate) home: String,
}

struct RuntimeOwner {
    runtime: Mutex<Option<tokio::runtime::Runtime>>,
    remote: Arc<Remote>,
    keepalive: Mutex<Option<Box<dyn Any + Send>>>,
    config: ClientConfig,
}

impl Drop for RuntimeOwner {
    fn drop(&mut self) {
        self.remote.closed.store(true, Ordering::SeqCst);
        if let Some(runtime) = self.runtime.lock().ok().and_then(|mut r| r.take()) {
            runtime.shutdown_background();
        }
        // Dropping the keep-alive last stops the transport thread.
        self.keepalive.lock().ok().and_then(|mut k| k.take());
    }
}

/// Handle to one SFTP connection. Cheap to clone.
#[derive(Clone)]
pub struct SftpClient {
    owner: Arc<RuntimeOwner>,
}

impl std::fmt::Debug for SftpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SftpClient")
    }
}

impl SftpClient {
    /// Wraps an authenticated raw `russh-sftp` session. `keepalive` is held
    /// for the client's lifetime (for `festerm-ssh` pass the
    /// `SftpTransportKeepAlive` from `SftpSession::into_transport`).
    pub fn from_russh(
        session: SftpSession,
        keepalive: impl Send + 'static,
        config: ClientConfig,
    ) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("festerm-sftp")
            .enable_all()
            .build()
            .map_err(|e| SftpError::Io(format!("sftp runtime: {e}")))?;
        session.set_timeout(config.request_timeout.as_secs().max(1));
        let home = runtime
            .block_on(async {
                tokio::time::timeout(config.operation_timeout, session.canonicalize(".")).await
            })
            .map_err(|_| SftpError::Timeout("resolving the home directory".into()))?
            .map_err(|e| SftpError::from_remote(&e, "resolve home directory"))?;
        let remote = Arc::new(Remote {
            session,
            closed: AtomicBool::new(false),
            home,
        });
        Ok(Self {
            owner: Arc::new(RuntimeOwner {
                runtime: Mutex::new(Some(runtime)),
                remote,
                keepalive: Mutex::new(Some(Box::new(keepalive))),
                config,
            }),
        })
    }

    pub(crate) fn remote(&self) -> Arc<Remote> {
        Arc::clone(&self.owner.remote)
    }

    pub(crate) fn handle(&self) -> Result<tokio::runtime::Handle> {
        self.owner
            .runtime
            .lock()
            .ok()
            .and_then(|r| r.as_ref().map(|r| r.handle().clone()))
            .ok_or_else(|| SftpError::Disconnected("client closed".into()))
    }

    /// False after [`Self::close`] or once a transport failure was observed.
    pub fn is_connected(&self) -> bool {
        !self.owner.remote.closed.load(Ordering::SeqCst)
    }

    /// Closes the SFTP channel; in-flight calls fail with `Disconnected`.
    pub fn close(&self) {
        let remote = self.remote();
        if remote.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Ok(handle) = self.handle() {
            handle.spawn(async move {
                let _ = remote.session.close().await;
            });
        }
    }

    /// Runs `future` on the client runtime and blocks for its result.
    fn run<T, F>(&self, future: F) -> Result<T>
    where
        T: Send + 'static,
        F: Future<Output = Result<T>> + Send + 'static,
    {
        let remote = self.remote();
        if remote.closed.load(Ordering::SeqCst) {
            return Err(SftpError::Disconnected("client closed".into()));
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        self.handle()?.spawn(async move {
            let result = future.await;
            if let Err(error) = &result {
                if error.is_connection_lost() {
                    remote.closed.store(true, Ordering::SeqCst);
                }
            }
            let _ = sender.send(result);
        });
        let timeout = self.owner.config.operation_timeout;
        match receiver.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(SftpError::Timeout("remote operation".into()))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(SftpError::Disconnected("worker stopped".into()))
            }
        }
    }
}

fn mode_kind(mode: Option<u32>, longname: &str) -> EntryKind {
    match mode.map(|m| m & 0o170000) {
        Some(0o040000) => EntryKind::Directory,
        Some(0o100000) => EntryKind::File,
        Some(0o120000) => EntryKind::Symlink,
        Some(0) | None => match longname.chars().next() {
            Some('d') => EntryKind::Directory,
            Some('-') => EntryKind::File,
            Some('l') => EntryKind::Symlink,
            _ => EntryKind::Other,
        },
        Some(_) => EntryKind::Other,
    }
}

pub(crate) fn entry_from_attrs(name: &str, attrs: &FileAttributes, longname: &str) -> Entry {
    Entry {
        name: name.to_owned(),
        display_name: path::display_name(name),
        kind: mode_kind(attrs.permissions, longname),
        size: attrs.size,
        modified: attrs.mtime.map(i64::from),
        permissions: attrs.permissions.map(|p| p & 0o7777),
        symlink: None,
        hidden: name.starts_with('.'),
        name_safe: path::name_problem(name).is_none(),
    }
}

async fn cancellable<T>(
    cancel: Option<&CancelToken>,
    future: impl Future<Output = T>,
) -> Result<T> {
    match cancel {
        None => Ok(future.await),
        Some(token) => tokio::select! {
            biased;
            _ = token.cancelled() => Err(SftpError::Cancelled),
            value = future => Ok(value),
        },
    }
}

fn status_is(error: &RawError, code: StatusCode) -> bool {
    matches!(error, RawError::Status(status) if status.status_code == code)
}

fn remote_err<T>(result: SftpResult<T>, context: &str) -> Result<T> {
    result.map_err(|e| SftpError::from_remote(&e, context))
}

fn push_ssh_string(buffer: &mut Vec<u8>, value: &str) {
    buffer.extend_from_slice(&(value.len() as u32).to_be_bytes());
    buffer.extend_from_slice(value.as_bytes());
}

impl Remote {
    fn raw(&self) -> Arc<RawSftpSession> {
        self.session.raw_session()
    }

    pub(crate) fn check_open(&self) -> Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            Err(SftpError::Disconnected("client closed".into()))
        } else {
            Ok(())
        }
    }

    /// Attributes of the entry itself, or `None` if it does not exist.
    pub(crate) async fn lstat(&self, remote_path: &str) -> Result<Option<FileAttributes>> {
        match self.raw().lstat(remote_path).await {
            Ok(attrs) => Ok(Some(attrs.attrs)),
            Err(error) if status_is(&error, StatusCode::NoSuchFile) => Ok(None),
            Err(error) => Err(SftpError::from_remote(&error, remote_path)),
        }
    }

    async fn resolve_symlink(&self, remote_path: &str) -> (SymlinkInfo, Option<FileAttributes>) {
        let raw = self.raw();
        let target = raw
            .readlink(remote_path)
            .await
            .ok()
            .and_then(|n| n.files.into_iter().next().map(|f| f.filename));
        match raw.stat(remote_path).await {
            Ok(attrs) => (
                SymlinkInfo {
                    target_kind: Some(mode_kind(attrs.attrs.permissions, "")),
                    target,
                    escapes_root: false,
                },
                Some(attrs.attrs),
            ),
            Err(_) => (
                SymlinkInfo {
                    target_kind: None,
                    target,
                    escapes_root: false,
                },
                None,
            ),
        }
    }

    pub(crate) async fn stat_entry(&self, remote_path: &str) -> Result<Entry> {
        let remote_path = path::normalize_remote(remote_path)?;
        let attrs = self
            .lstat(&remote_path)
            .await?
            .ok_or_else(|| SftpError::NotFound(remote_path.clone()))?;
        let name = remote_path.rsplit('/').next().unwrap_or("").to_owned();
        let mut entry = entry_from_attrs(&name, &attrs, "");
        if entry.kind == EntryKind::Symlink {
            let (info, followed) = self.resolve_symlink(&remote_path).await;
            if let Some(followed) = followed {
                entry.size = followed.size;
                entry.modified = followed.mtime.map(i64::from);
            }
            entry.symlink = Some(info);
        }
        Ok(entry)
    }

    /// Bounded raw directory enumeration: `(filename, attrs, longname)`.
    async fn read_directory(
        &self,
        remote_path: &str,
        limit: usize,
        cancel: Option<&CancelToken>,
    ) -> Result<(Vec<(String, FileAttributes, String)>, bool)> {
        let raw = self.raw();
        let handle = cancellable(cancel, raw.opendir(remote_path))
            .await?
            .map_err(|e| SftpError::from_remote(&e, remote_path))?
            .handle;
        let result = self.read_handle(&raw, &handle, limit, cancel).await;
        let _ = raw.close(handle).await;
        result
    }

    async fn read_handle(
        &self,
        raw: &RawSftpSession,
        handle: &str,
        limit: usize,
        cancel: Option<&CancelToken>,
    ) -> Result<(Vec<(String, FileAttributes, String)>, bool)> {
        let mut entries = Vec::new();
        let mut truncated = false;
        // A hostile server could answer empty pages forever.
        let mut rounds = 0usize;
        'pages: loop {
            rounds += 1;
            if rounds > limit + 64 {
                return Err(SftpError::Remote(
                    "directory listing did not terminate".into(),
                ));
            }
            match cancellable(cancel, raw.readdir(handle)).await? {
                Ok(page) => {
                    for file in page.files {
                        if file.filename == "." || file.filename == ".." {
                            continue;
                        }
                        if entries.len() >= limit {
                            truncated = true;
                            break 'pages;
                        }
                        entries.push((file.filename, file.attrs, file.longname));
                    }
                }
                Err(error) if status_is(&error, StatusCode::Eof) => break,
                Err(error) => return Err(SftpError::from_remote(&error, "read directory")),
            }
        }
        Ok((entries, truncated))
    }

    pub(crate) async fn list(
        self: &Arc<Self>,
        remote_path: &str,
        options: &ListOptions,
    ) -> Result<Listing> {
        let remote_path = path::normalize_remote(remote_path)?;
        let (raw_entries, truncated) = self
            .read_directory(&remote_path, options.scan_limit(), options.cancel.as_ref())
            .await?;
        let mut entries: Vec<Entry> = raw_entries
            .iter()
            .map(|(name, attrs, longname)| entry_from_attrs(name, attrs, longname))
            .collect();

        let mut tasks = tokio::task::JoinSet::new();
        for (index, entry) in entries.iter().enumerate() {
            if entry.kind != EntryKind::Symlink || !entry.name_safe {
                continue;
            }
            if tasks.len() >= MAX_SYMLINK_RESOLVES {
                break;
            }
            let remote = Arc::clone(self);
            let link_path = path::join_remote(&remote_path, &entry.name)?;
            tasks.spawn(async move { (index, remote.resolve_symlink(&link_path).await) });
        }
        loop {
            let next = cancellable(options.cancel.as_ref(), tasks.join_next()).await?;
            match next {
                Some(Ok((index, (info, followed)))) => {
                    let entry = &mut entries[index];
                    if let Some(followed) = followed {
                        entry.size = followed.size;
                        entry.modified = followed.mtime.map(i64::from);
                    }
                    entry.symlink = Some(info);
                }
                Some(Err(_)) => {}
                None => break,
            }
        }
        for entry in &mut entries {
            if entry.kind == EntryKind::Symlink && entry.symlink.is_none() {
                entry.symlink = Some(SymlinkInfo {
                    target_kind: None,
                    target: None,
                    escapes_root: false,
                });
            }
        }
        Ok(finish_listing(remote_path, entries, truncated, options))
    }

    pub(crate) async fn mkdir(&self, remote_path: &str) -> Result<()> {
        let remote_path = path::normalize_remote(remote_path)?;
        path::file_name_remote(&remote_path)?;
        match self
            .raw()
            .mkdir(remote_path.as_str(), FileAttributes::empty())
            .await
        {
            Ok(_) => Ok(()),
            Err(error) => {
                if matches!(self.lstat(&remote_path).await, Ok(Some(_))) {
                    Err(SftpError::AlreadyExists(remote_path))
                } else {
                    Err(SftpError::from_remote(&error, &remote_path))
                }
            }
        }
    }

    pub(crate) async fn rename_no_replace(&self, from: &str, to: &str) -> Result<()> {
        let from = path::normalize_remote(from)?;
        let to = path::normalize_remote(to)?;
        path::file_name_remote(&to)?;
        if self.lstat(&to).await?.is_some() {
            return Err(SftpError::AlreadyExists(to));
        }
        match self.raw().rename(from.as_str(), to.as_str()).await {
            Ok(_) => Ok(()),
            // Some servers refuse to overwrite and report a generic failure.
            Err(error) if self.lstat(&to).await.ok().flatten().is_some() => {
                let _ = error;
                Err(SftpError::AlreadyExists(to))
            }
            Err(error) => Err(SftpError::from_remote(&error, &from)),
        }
    }

    /// Atomic replace via `posix-rename@openssh.com`; servers without it fall
    /// back to remove-then-rename (a short window where only the temporary
    /// file holds the data, which is kept on failure).
    pub(crate) async fn rename_replace(&self, from: &str, to: &str) -> Result<()> {
        let raw = self.raw();
        let mut data = Vec::new();
        push_ssh_string(&mut data, from);
        push_ssh_string(&mut data, to);
        match raw.extended("posix-rename@openssh.com", data).await {
            Ok(Packet::Status(status)) if status.status_code == StatusCode::Ok => return Ok(()),
            Err(RawError::Status(_)) => {}
            Ok(_) | Err(_) => {
                return Err(SftpError::Disconnected("posix-rename failed".into()));
            }
        }
        if let Some(existing) = self.lstat(to).await? {
            if mode_kind(existing.permissions, "") == EntryKind::Directory {
                return Err(SftpError::IsADirectory(to.to_owned()));
            }
            remote_err(raw.remove(to).await, to)?;
        }
        remote_err(raw.rename(from, to).await, from)?;
        Ok(())
    }

    pub(crate) async fn remove_file(&self, remote_path: &str) -> Result<()> {
        remote_err(self.raw().remove(remote_path).await, remote_path).map(|_| ())
    }

    async fn delete_tree(
        &self,
        remote_path: &str,
        depth: usize,
        budget: &mut usize,
        really_delete: bool,
    ) -> Result<()> {
        if depth > MAX_DELETE_DEPTH {
            return Err(SftpError::LimitExceeded("directory tree too deep".into()));
        }
        let (children, truncated) = self.read_directory(remote_path, *budget, None).await?;
        if truncated {
            return Err(SftpError::LimitExceeded(
                "too many entries to delete at once".into(),
            ));
        }
        *budget = budget.saturating_sub(children.len());
        for (name, attrs, longname) in children {
            path::validate_name(&name)?;
            let child = path::join_remote(remote_path, &name)?;
            if mode_kind(attrs.permissions, &longname) == EntryKind::Directory {
                Box::pin(self.delete_tree(&child, depth + 1, budget, really_delete)).await?;
                if really_delete {
                    remote_err(self.raw().rmdir(child.as_str()).await, &child)?;
                }
            } else if really_delete {
                self.remove_file(&child).await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn delete(&self, remote_path: &str, recursive: bool) -> Result<()> {
        let remote_path = path::normalize_remote(remote_path)?;
        if remote_path == "/" || remote_path == "." {
            return Err(SftpError::InvalidPath("refusing to delete the root".into()));
        }
        let attrs = self
            .lstat(&remote_path)
            .await?
            .ok_or_else(|| SftpError::NotFound(remote_path.clone()))?;
        if mode_kind(attrs.permissions, "") != EntryKind::Directory {
            return self.remove_file(&remote_path).await;
        }
        if recursive {
            let mut budget = MAX_DELETE_ENTRIES;
            self.delete_tree(&remote_path, 1, &mut budget, false)
                .await?;
            let mut budget = MAX_DELETE_ENTRIES;
            self.delete_tree(&remote_path, 1, &mut budget, true).await?;
        }
        match self.raw().rmdir(remote_path.as_str()).await {
            Ok(_) => Ok(()),
            Err(error) => {
                let not_empty = self
                    .read_directory(&remote_path, 1, None)
                    .await
                    .map(|(entries, _)| !entries.is_empty())
                    .unwrap_or(false);
                if not_empty {
                    Err(SftpError::DirectoryNotEmpty(remote_path))
                } else {
                    Err(SftpError::from_remote(&error, &remote_path))
                }
            }
        }
    }
}

impl PaneFs for SftpClient {
    fn kind(&self) -> PaneKind {
        PaneKind::Remote
    }

    fn home(&self) -> String {
        self.owner.remote.home.clone()
    }

    fn list(&self, remote_path: &str, options: &ListOptions) -> Result<Listing> {
        let remote = self.remote();
        let (remote_path, options) = (remote_path.to_owned(), options.clone());
        self.run(async move { remote.list(&remote_path, &options).await })
    }

    fn stat(&self, remote_path: &str) -> Result<Entry> {
        let remote = self.remote();
        let remote_path = remote_path.to_owned();
        self.run(async move { remote.stat_entry(&remote_path).await })
    }

    fn mkdir(&self, remote_path: &str) -> Result<()> {
        let remote = self.remote();
        let remote_path = remote_path.to_owned();
        self.run(async move { remote.mkdir(&remote_path).await })
    }

    fn rename(&self, from: &str, to: &str) -> Result<()> {
        let remote = self.remote();
        let (from, to) = (from.to_owned(), to.to_owned());
        self.run(async move { remote.rename_no_replace(&from, &to).await })
    }

    fn delete(&self, remote_path: &str, recursive: bool) -> Result<()> {
        let remote = self.remote();
        let remote_path = remote_path.to_owned();
        self.run(async move { remote.delete(&remote_path, recursive).await })
    }

    fn join(&self, directory: &str, name: &str) -> Result<String> {
        path::join_remote(directory, name)
    }

    fn parent(&self, remote_path: &str) -> Option<String> {
        path::parent_remote(remote_path)
    }
}
