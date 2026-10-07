use std::{
    env, fmt,
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

use crate::sftp_planning::{Budgeted, PlanningQueue, SharedPlanningBudget};
use crate::sftp_transfer::{
    path_memory_proxy_bytes, SftpDirectoryItem, SftpDirectorySnapshot, SftpLocation, SftpPath,
    SftpPathMetadata, TransferWorkError,
};
use russh_sftp::{
    client::SftpSession as RusshSftpSession,
    protocol::{FileType as RusshSftpFileType, OpenFlags},
};
use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
};

const HELP_TEXT: &str = "\
Supported commands:
  help
  pwd
  lpwd
  cd <remote-directory>
  lcd <local-directory>
  ls [-al] [remote-path]
  dir [-al] [remote-path]
  lls [-al]
  ldir [-al]
  mkdir <remote-directory>
  rmdir <remote-directory>
  rm <remote-path>
  rename <old-remote-path> <new-remote-path>
  chmod <octal-mode> <remote-path>
  get <remote-path> [local-destination]
  put <local-path> [remote-destination]
  quit
  exit

Command input is limited to 256 KiB (262144 bytes) per unfinished line.
Overlong lines are refused until a line ending or Ctrl+C; then start a new command.

Not supported in this first pass: reget, reput, symlink, chown, shell escapes,
recursive -r transfers, and globbing/wildcard expansion.";

const TRANSFER_CHUNK_BYTES: usize = 64 * 1024;
pub const SFTP_CANCELLATION_CLEANUP_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(2);

/// Parsed text-mode SFTP command supported by fesTerm.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SftpCommand {
    Help,
    Pwd,
    Lpwd,
    Cd {
        path: String,
    },
    Lcd {
        path: String,
    },
    Ls {
        path: Option<String>,
    },
    Lls,
    Mkdir {
        path: String,
    },
    Rmdir {
        path: String,
    },
    Rm {
        path: String,
    },
    Rename {
        source: String,
        destination: String,
    },
    Chmod {
        mode: u32,
        path: String,
    },
    Get {
        source: String,
        destination: Option<String>,
    },
    Put {
        source: String,
        destination: Option<String>,
    },
    Quit,
}

/// Outcome of running one SFTP command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SftpCommandOutcome {
    Help {
        text: &'static str,
    },
    WorkingDirectory {
        path: String,
    },
    LocalWorkingDirectory {
        path: PathBuf,
    },
    ChangedDirectory {
        path: String,
    },
    ChangedLocalDirectory {
        path: PathBuf,
    },
    DirectoryListing {
        path: String,
        entries: Vec<SftpDirectoryEntry>,
    },
    LocalDirectoryListing {
        path: PathBuf,
        entries: Vec<SftpDirectoryEntry>,
    },
    CreatedDirectory {
        path: String,
    },
    RemovedDirectory {
        path: String,
    },
    RemovedFile {
        path: String,
    },
    Renamed {
        source: String,
        destination: String,
    },
    PermissionsChanged {
        path: String,
        mode: u32,
    },
    Downloaded {
        remote_path: String,
        local_path: PathBuf,
        byte_count: u64,
    },
    Uploaded {
        local_path: PathBuf,
        remote_path: String,
        byte_count: u64,
    },
    SessionClosed,
}

/// One `ls` entry exposed to a later transcript/UI layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SftpDirectoryEntry {
    pub name: String,
    pub path: String,
    pub file_type: SftpEntryType,
    pub size: Option<u64>,
    pub permissions: Option<u32>,
}

/// Simplified file type for text-mode directory listings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SftpEntryType {
    Directory,
    File,
    Symlink,
    Other,
}

impl From<RusshSftpFileType> for SftpEntryType {
    fn from(value: RusshSftpFileType) -> Self {
        match value {
            RusshSftpFileType::Dir => Self::Directory,
            RusshSftpFileType::File => Self::File,
            RusshSftpFileType::Symlink => Self::Symlink,
            RusshSftpFileType::Other => Self::Other,
        }
    }
}

/// Content-free parser/validation errors for text-mode SFTP commands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SftpCommandParseError {
    Empty,
    UnterminatedQuote,
    UnsupportedCommand {
        command: String,
    },
    UnsupportedFeature {
        feature: &'static str,
    },
    InvalidArguments {
        command: &'static str,
        usage: &'static str,
    },
    InvalidMode {
        value: String,
    },
}

impl fmt::Display for SftpCommandParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("SFTP command must not be empty"),
            Self::UnterminatedQuote => {
                formatter.write_str("SFTP command has an unterminated quote")
            }
            Self::UnsupportedCommand { command } => {
                write!(formatter, "unsupported SFTP command: {command}")
            }
            Self::UnsupportedFeature { feature } => write!(formatter, "{feature} is not supported"),
            Self::InvalidArguments { command, usage } => {
                write!(formatter, "invalid arguments for {command}; usage: {usage}")
            }
            Self::InvalidMode { value } => {
                write!(formatter, "chmod mode must be octal digits, got {value}")
            }
        }
    }
}

impl std::error::Error for SftpCommandParseError {}

/// Content-free SFTP backend error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SftpSessionError {
    CommandParse(SftpCommandParseError),
    SessionClosed,
    EmptyRemotePath,
    EmptyLocalPath,
    LocalDirectoryUnavailable {
        path: String,
        reason: String,
    },
    LocalPathNotDirectory {
        path: String,
    },
    RemotePathNotDirectory {
        path: String,
    },
    DestinationExists {
        path: String,
    },
    MissingFileName {
        path: String,
    },
    RemoteFileTooLarge {
        path: String,
        limit_bytes: usize,
    },
    SubsystemRejected,
    PartialFileOwnershipUnconfirmed {
        path: String,
    },
    CancellationCleanupTimedOut,
    TransferCommitInterrupted {
        temporary: String,
        destination: String,
    },
    TransferFailedWithIncompleteCleanup {
        operation_error: Box<SftpSessionError>,
        cleanup_error: Box<SftpSessionError>,
    },
    LocalOperationFailed {
        operation: &'static str,
        path: String,
        reason: String,
    },
    RemoteOperationFailed {
        operation: &'static str,
        path: String,
        reason: String,
    },
    RemotePairOperationFailed {
        operation: &'static str,
        source: String,
        destination: String,
        reason: String,
    },
}

impl fmt::Display for SftpSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CommandParse(error) => error.fmt(formatter),
            Self::SessionClosed => formatter.write_str("SFTP session is already closed"),
            Self::EmptyRemotePath => formatter.write_str("remote path must not be empty"),
            Self::EmptyLocalPath => formatter.write_str("local path must not be empty"),
            Self::LocalDirectoryUnavailable { path, reason } => {
                write!(formatter, "local directory {path} is unavailable: {reason}")
            }
            Self::LocalPathNotDirectory { path } => {
                write!(formatter, "local path is not a directory: {path}")
            }
            Self::RemotePathNotDirectory { path } => {
                write!(formatter, "remote path is not a directory: {path}")
            }
            Self::DestinationExists { path } => {
                write!(formatter, "destination already exists: {path}")
            }
            Self::MissingFileName { path } => {
                write!(formatter, "path has no usable file name: {path}")
            }
            Self::RemoteFileTooLarge { path, limit_bytes } => {
                write!(
                    formatter,
                    "remote file exceeds the {limit_bytes}-byte limit: {path}"
                )
            }
            Self::SubsystemRejected => {
                formatter.write_str("SSH server rejected the SFTP subsystem request")
            }
            Self::PartialFileOwnershipUnconfirmed { path } => write!(
                formatter,
                "partial-file ownership could not be confirmed; file was not removed: {path}"
            ),
            Self::CancellationCleanupTimedOut => formatter.write_str(
                "SFTP cancellation cleanup exceeded two seconds; partial output may remain",
            ),
            Self::TransferCommitInterrupted { temporary, destination } => write!(
                formatter,
                "transfer commit could not be confirmed; temporary output was not removed; inspect temporary and destination paths: {temporary} -> {destination}"
            ),
            Self::TransferFailedWithIncompleteCleanup { operation_error, cleanup_error } => {
                write!(formatter, "{operation_error}; cleanup is incomplete: {cleanup_error}")
            }
            Self::LocalOperationFailed {
                operation,
                path,
                reason,
            } => write!(formatter, "local {operation} failed for {path}: {reason}"),
            Self::RemoteOperationFailed {
                operation,
                path,
                reason,
            } => write!(formatter, "remote {operation} failed for {path}: {reason}"),
            Self::RemotePairOperationFailed {
                operation,
                source,
                destination,
                reason,
            } => write!(
                formatter,
                "remote {operation} failed for {source} -> {destination}: {reason}"
            ),
        }
    }
}

impl std::error::Error for SftpSessionError {}

impl From<SftpCommandParseError> for SftpSessionError {
    fn from(value: SftpCommandParseError) -> Self {
        Self::CommandParse(value)
    }
}

/// Text-mode SFTP backend built on an authenticated `russh` handle and
/// `russh-sftp`.
pub struct SftpSession {
    client: RusshSftpSession,
    remote_working_directory: String,
    local_working_directory: PathBuf,
    closed: bool,
    partial_file: Option<PartialFile>,
    /// Keeps the connection's background transport task alive for as long
    /// as this session lives. Only ever set by
    /// [`Self::with_runtime_keepalive`], used exclusively by
    /// `connect_gui_sftp_session` in `festerm-ssh::lib`; the terminal SFTP
    /// path (`SftpTerminalSession`) doesn't need this because its dedicated
    /// worker thread's `tokio` runtime already spans the whole session, not
    /// just the connect phase.
    runtime_keepalive: Option<GuiSftpConnectionKeepAlive>,
}

struct PartialFile {
    path: SftpPath,
    owned: bool,
    commit_destination: Option<SftpPath>,
}

struct RemotePlanningDirectory {
    session: std::sync::Arc<russh_sftp::client::RawSftpSession>,
    handle: Option<String>,
    runtime: tokio::runtime::Handle,
}

impl RemotePlanningDirectory {
    async fn close(&mut self) -> russh_sftp::client::rawsession::SftpResult<()> {
        let handle = self
            .handle
            .take()
            .expect("remote planning directory is open");
        self.session.close(handle).await.map(|_| ())
    }
}

impl Drop for RemotePlanningDirectory {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let session = std::sync::Arc::clone(&self.session);
            self.runtime.spawn(async move {
                if let Err(error) = session.close(handle).await {
                    tracing::warn!(
                        target: "festerm::sftp",
                        "Cancelled SFTP directory enumeration could not close its handle"
                    );
                    eprintln!("fesTerm: SFTP directory close failed: {error}");
                }
            });
        }
    }
}

/// Drop guard that keeps a dedicated background thread's `tokio` runtime
/// alive -- and therefore keeps polling whatever `russh` spawned onto it
/// while connecting -- for as long as the [`SftpSession`] it's attached to
/// lives.
///
/// `russh::client::connect` spawns the connection's entire read/write
/// dispatch loop via `tokio::spawn`, onto whatever runtime happens to be
/// ambient at the time. A connect path that builds a throw-away runtime
/// just to `block_on` the connect handshake and then returns will drop that
/// runtime as soon as it returns -- which cancels the just-spawned dispatch
/// task, silently killing the connection a moment after it reports
/// "connected". This guard's `Drop` signals the owning thread to stop
/// keeping its runtime alive once (and only once) the session itself is
/// dropped.
pub(crate) struct GuiSftpConnectionKeepAlive {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl GuiSftpConnectionKeepAlive {
    pub(crate) fn new(shutdown: tokio::sync::oneshot::Sender<()>) -> Self {
        Self {
            shutdown: Some(shutdown),
        }
    }
}

impl Drop for GuiSftpConnectionKeepAlive {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

impl SftpSession {
    /// Attaches a [`GuiSftpConnectionKeepAlive`] guard to this session,
    /// tying the guard's (and therefore its owning thread's runtime's)
    /// lifetime to this session's.
    pub(crate) fn with_runtime_keepalive(mut self, guard: GuiSftpConnectionKeepAlive) -> Self {
        self.runtime_keepalive = Some(guard);
        self
    }

    /// Opens a dedicated SSH session channel on `handle`, requests the
    /// `"sftp"` subsystem, and starts the SFTP protocol client.
    pub async fn connect<H>(handle: &russh::client::Handle<H>) -> Result<Self, SftpSessionError>
    where
        H: russh::client::Handler + Send + 'static,
    {
        let local_working_directory =
            env::current_dir().map_err(|error| SftpSessionError::LocalDirectoryUnavailable {
                path: ".".to_owned(),
                reason: error.to_string(),
            })?;
        Self::connect_with_local_directory(handle, local_working_directory).await
    }

    /// Like [`Self::connect`] but with an explicit starting local directory.
    pub async fn connect_with_local_directory<H>(
        handle: &russh::client::Handle<H>,
        local_working_directory: impl Into<PathBuf>,
    ) -> Result<Self, SftpSessionError>
    where
        H: russh::client::Handler + Send + 'static,
    {
        let mut channel = handle.channel_open_session().await.map_err(|error| {
            SftpSessionError::RemoteOperationFailed {
                operation: "open session channel",
                path: "<sftp>".to_owned(),
                reason: error.to_string(),
            }
        })?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|error| SftpSessionError::RemoteOperationFailed {
                operation: "request subsystem",
                path: "sftp".to_owned(),
                reason: error.to_string(),
            })?;
        wait_for_subsystem_acceptance(&mut channel).await?;

        let client = RusshSftpSession::new(channel.into_stream())
            .await
            .map_err(|error| SftpSessionError::RemoteOperationFailed {
                operation: "initialize SFTP session",
                path: "<sftp>".to_owned(),
                reason: error.to_string(),
            })?;

        let remote_working_directory = client.canonicalize(".").await.map_err(|error| {
            SftpSessionError::RemoteOperationFailed {
                operation: "resolve working directory",
                path: ".".to_owned(),
                reason: error.to_string(),
            }
        })?;
        let local_working_directory =
            prepare_local_working_directory(local_working_directory.into()).await?;

        Ok(Self {
            client,
            remote_working_directory,
            local_working_directory,
            closed: false,
            partial_file: None,
            runtime_keepalive: None,
        })
    }

    fn begin_partial_file(&mut self, path: SftpPath) -> Result<(), SftpSessionError> {
        if let Some(partial) = &self.partial_file {
            return Err(SftpSessionError::LocalOperationFailed {
                operation: "start transfer",
                path: partial.path.display(),
                reason: "previous partial output still requires cleanup".to_owned(),
            });
        }
        self.partial_file = Some(PartialFile {
            path,
            owned: false,
            commit_destination: None,
        });
        Ok(())
    }

    fn confirm_partial_file(&mut self) {
        self.partial_file
            .as_mut()
            .expect("destination creation has a pending partial file")
            .owned = true;
    }

    pub(crate) fn forget_partial_file(&mut self, path: &SftpPath) {
        if self
            .partial_file
            .as_ref()
            .is_some_and(|partial| &partial.path == path)
        {
            self.partial_file = None;
        }
    }

    pub(crate) fn protect_partial_commit(&mut self, temporary: &SftpPath, destination: &SftpPath) {
        if let Some(partial) = self.partial_file.as_mut() {
            if &partial.path == temporary {
                partial.commit_destination = Some(destination.clone());
            }
        }
    }

    pub(crate) async fn cleanup_interrupted_transfer(&mut self) -> Result<(), SftpSessionError> {
        let Some(partial) = self.partial_file.take() else {
            return Ok(());
        };
        if partial.owned {
            if let Some(destination) = partial.commit_destination {
                return Err(SftpSessionError::TransferCommitInterrupted {
                    temporary: partial.path.display(),
                    destination: destination.display(),
                });
            }
        }
        // Creation acknowledgement cannot prove what a mutable path names now.
        Err(SftpSessionError::PartialFileOwnershipUnconfirmed {
            path: partial.path.display(),
        })
    }

    async fn finish_failed_transfer(&mut self, error: SftpSessionError) -> SftpSessionError {
        match self.cleanup_interrupted_transfer().await {
            Ok(()) => error,
            Err(cleanup_error) => SftpSessionError::TransferFailedWithIncompleteCleanup {
                operation_error: Box::new(error),
                cleanup_error: Box::new(cleanup_error),
            },
        }
    }

    /// Parses and runs one text-mode SFTP command line.
    pub async fn execute_line(
        &mut self,
        line: &str,
    ) -> Result<SftpCommandOutcome, SftpSessionError> {
        self.execute(parse_sftp_command(line)?).await
    }

    /// Runs one already-parsed SFTP command.
    pub async fn execute(
        &mut self,
        command: SftpCommand,
    ) -> Result<SftpCommandOutcome, SftpSessionError> {
        match command {
            SftpCommand::Help => Ok(SftpCommandOutcome::Help { text: HELP_TEXT }),
            SftpCommand::Pwd => Ok(SftpCommandOutcome::WorkingDirectory {
                path: self.remote_working_directory.clone(),
            }),
            SftpCommand::Lpwd => Ok(SftpCommandOutcome::LocalWorkingDirectory {
                path: self.local_working_directory.clone(),
            }),
            SftpCommand::Cd { path } => self.cd(&path).await,
            SftpCommand::Lcd { path } => self.lcd(&path).await,
            SftpCommand::Ls { path } => self.ls(path.as_deref()).await,
            SftpCommand::Lls => self.lls().await,
            SftpCommand::Mkdir { path } => self.mkdir(&path).await,
            SftpCommand::Rmdir { path } => self.rmdir(&path).await,
            SftpCommand::Rm { path } => self.rm(&path).await,
            SftpCommand::Rename {
                source,
                destination,
            } => self.rename(&source, &destination).await,
            SftpCommand::Chmod { mode, path } => self.chmod(mode, &path).await,
            SftpCommand::Get {
                source,
                destination,
            } => self.get(&source, destination.as_deref()).await,
            SftpCommand::Put {
                source,
                destination,
            } => self.put(&source, destination.as_deref()).await,
            SftpCommand::Quit => self.close().await,
        }
    }

    /// Returns the tracked remote working directory.
    pub fn remote_working_directory(&self) -> &str {
        &self.remote_working_directory
    }

    /// Returns the tracked local working directory.
    pub fn local_working_directory(&self) -> &Path {
        &self.local_working_directory
    }

    /// Captures a sortable snapshot of the current local working directory.
    pub async fn current_local_directory_snapshot(
        &self,
    ) -> Result<SftpDirectorySnapshot, SftpSessionError> {
        read_local_directory_snapshot(&self.local_working_directory).await
    }

    /// Captures a sortable snapshot of `path` on the local filesystem.
    pub async fn local_directory_snapshot(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<SftpDirectorySnapshot, SftpSessionError> {
        read_local_directory_snapshot(path.as_ref()).await
    }

    /// Captures a sortable snapshot of the current remote working directory or
    /// an explicit remote directory path.
    pub async fn remote_directory_snapshot(
        &mut self,
        path: Option<&str>,
    ) -> Result<SftpDirectorySnapshot, SftpSessionError> {
        self.ensure_open()?;
        let resolved = match path {
            Some(path) => resolve_remote_path(&self.remote_working_directory, path)?,
            None => self.remote_working_directory.clone(),
        };
        self.remote_directory_snapshot_exact(&resolved).await
    }

    /// Reads local metadata without requiring a transfer queue.
    pub async fn local_path_metadata(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<Option<SftpPathMetadata>, SftpSessionError> {
        read_local_path_metadata(path.as_ref()).await
    }

    /// Reads remote metadata without requiring a transfer queue.
    pub async fn remote_path_metadata(
        &mut self,
        path: &str,
    ) -> Result<Option<SftpPathMetadata>, SftpSessionError> {
        self.ensure_open()?;
        let resolved = resolve_remote_path(&self.remote_working_directory, path)?;
        self.remote_path_metadata_exact(&resolved).await
    }

    /// Reads one remote file snapshot up to `max_bytes`, reusing the existing
    /// authenticated SFTP session rather than opening a second SSH path.
    pub async fn read_markdown_snapshot(
        &mut self,
        path: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, SftpSessionError> {
        self.ensure_open()?;
        let resolved = resolve_remote_path(&self.remote_working_directory, path)?;
        self.read_file_snapshot_exact(&resolved, max_bytes).await
    }

    pub(crate) async fn read_file_snapshot_exact(
        &mut self,
        path: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, SftpSessionError> {
        self.ensure_open()?;
        let resolved = path.to_owned();
        let mut file = self
            .client
            .open(resolved.clone())
            .await
            .map_err(|error| remote_error("open file", &resolved, error))?;
        let mut bytes = Vec::new();
        let mut buffer = vec![0u8; TRANSFER_CHUNK_BYTES.min(max_bytes.saturating_add(1).max(1))];
        loop {
            let read = file
                .read(&mut buffer)
                .await
                .map_err(|error| remote_error("read file", &resolved, error))?;
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            if bytes.len() > max_bytes {
                return Err(SftpSessionError::RemoteFileTooLarge {
                    path: resolved,
                    limit_bytes: max_bytes,
                });
            }
        }
        Ok(bytes)
    }

    /// Closes the SFTP subsystem channel cleanly.
    pub async fn close(&mut self) -> Result<SftpCommandOutcome, SftpSessionError> {
        if !self.closed {
            self.client
                .close()
                .await
                .map_err(|error| SftpSessionError::RemoteOperationFailed {
                    operation: "close session",
                    path: "<sftp>".to_owned(),
                    reason: error.to_string(),
                })?;
            self.closed = true;
        }
        Ok(SftpCommandOutcome::SessionClosed)
    }

    async fn cd(&mut self, path: &str) -> Result<SftpCommandOutcome, SftpSessionError> {
        self.ensure_open()?;
        let resolved = resolve_remote_path(&self.remote_working_directory, path)?;
        let canonical = self
            .client
            .canonicalize(resolved.clone())
            .await
            .map_err(|error| remote_error("change directory", &resolved, error))?;
        let metadata = self
            .client
            .metadata(canonical.clone())
            .await
            .map_err(|error| remote_error("change directory", &canonical, error))?;
        if !metadata.is_dir() {
            return Err(SftpSessionError::RemotePathNotDirectory { path: canonical });
        }
        self.remote_working_directory = canonical.clone();
        Ok(SftpCommandOutcome::ChangedDirectory { path: canonical })
    }

    async fn lcd(&mut self, path: &str) -> Result<SftpCommandOutcome, SftpSessionError> {
        let directory = prepare_local_working_directory(resolve_local_path(
            &self.local_working_directory,
            path,
        )?)
        .await?;
        self.local_working_directory = directory.clone();
        Ok(SftpCommandOutcome::ChangedLocalDirectory { path: directory })
    }

    async fn ls(&mut self, path: Option<&str>) -> Result<SftpCommandOutcome, SftpSessionError> {
        self.ensure_open()?;
        let resolved = match path {
            Some(path) => resolve_remote_path(&self.remote_working_directory, path)?,
            None => self.remote_working_directory.clone(),
        };
        let metadata = self
            .client
            .metadata(resolved.clone())
            .await
            .map_err(|error| remote_error("list path", &resolved, error))?;

        let mut entries = if metadata.is_dir() {
            self.client
                .read_dir(resolved.clone())
                .await
                .map_err(|error| remote_error("list path", &resolved, error))?
                .map(|entry| SftpDirectoryEntry {
                    name: entry.file_name(),
                    path: entry.path(),
                    file_type: entry.file_type().into(),
                    size: entry.metadata().size,
                    permissions: entry.metadata().permissions,
                })
                .collect::<Vec<_>>()
        } else {
            vec![SftpDirectoryEntry {
                name: remote_file_name(&resolved)?.to_owned(),
                path: resolved.clone(),
                file_type: metadata.file_type().into(),
                size: metadata.size,
                permissions: metadata.permissions,
            }]
        };
        entries.sort_by(|left, right| left.name.cmp(&right.name));

        Ok(SftpCommandOutcome::DirectoryListing {
            path: resolved,
            entries,
        })
    }

    async fn lls(&self) -> Result<SftpCommandOutcome, SftpSessionError> {
        let snapshot = self.current_local_directory_snapshot().await?;
        let path = match snapshot.path {
            SftpPath::Local(path) => path,
            SftpPath::Remote(_) => unreachable!("local directory snapshots retain local paths"),
        };
        let entries = snapshot
            .entries
            .into_iter()
            .map(|entry| SftpDirectoryEntry {
                name: entry.name,
                path: entry.path.display(),
                file_type: entry.file_type,
                size: entry.size,
                permissions: entry.permissions,
            })
            .collect();
        Ok(SftpCommandOutcome::LocalDirectoryListing { path, entries })
    }

    async fn mkdir(&mut self, path: &str) -> Result<SftpCommandOutcome, SftpSessionError> {
        self.ensure_open()?;
        let resolved = resolve_remote_path(&self.remote_working_directory, path)?;
        self.client
            .create_dir(resolved.clone())
            .await
            .map_err(|error| remote_error("create directory", &resolved, error))?;
        Ok(SftpCommandOutcome::CreatedDirectory { path: resolved })
    }

    async fn rmdir(&mut self, path: &str) -> Result<SftpCommandOutcome, SftpSessionError> {
        self.ensure_open()?;
        let resolved = resolve_remote_path(&self.remote_working_directory, path)?;
        self.client
            .remove_dir(resolved.clone())
            .await
            .map_err(|error| remote_error("remove directory", &resolved, error))?;
        Ok(SftpCommandOutcome::RemovedDirectory { path: resolved })
    }

    async fn rm(&mut self, path: &str) -> Result<SftpCommandOutcome, SftpSessionError> {
        self.ensure_open()?;
        let resolved = resolve_remote_path(&self.remote_working_directory, path)?;
        self.client
            .remove_file(resolved.clone())
            .await
            .map_err(|error| remote_error("remove file", &resolved, error))?;
        Ok(SftpCommandOutcome::RemovedFile { path: resolved })
    }

    async fn rename(
        &mut self,
        source: &str,
        destination: &str,
    ) -> Result<SftpCommandOutcome, SftpSessionError> {
        self.ensure_open()?;
        let source = resolve_remote_path(&self.remote_working_directory, source)?;
        let destination = resolve_remote_path(&self.remote_working_directory, destination)?;
        self.client
            .rename(source.clone(), destination.clone())
            .await
            .map_err(|error| SftpSessionError::RemotePairOperationFailed {
                operation: "rename",
                source: source.clone(),
                destination: destination.clone(),
                reason: error.to_string(),
            })?;
        Ok(SftpCommandOutcome::Renamed {
            source,
            destination,
        })
    }

    async fn chmod(
        &mut self,
        mode: u32,
        path: &str,
    ) -> Result<SftpCommandOutcome, SftpSessionError> {
        self.ensure_open()?;
        let resolved = resolve_remote_path(&self.remote_working_directory, path)?;
        let mut metadata = self
            .client
            .metadata(resolved.clone())
            .await
            .map_err(|error| remote_error("chmod", &resolved, error))?;
        let file_type_bits = metadata.permissions.unwrap_or_default() & 0o170000;
        metadata.permissions = Some(file_type_bits | mode);
        self.client
            .set_metadata(resolved.clone(), metadata)
            .await
            .map_err(|error| remote_error("chmod", &resolved, error))?;
        Ok(SftpCommandOutcome::PermissionsChanged {
            path: resolved,
            mode,
        })
    }

    async fn get(
        &mut self,
        source: &str,
        destination: Option<&str>,
    ) -> Result<SftpCommandOutcome, SftpSessionError> {
        self.ensure_open()?;
        let remote_path = resolve_remote_path(&self.remote_working_directory, source)?;
        let local_path = self
            .resolve_local_transfer_destination(&remote_path, destination)
            .await?;
        ensure_local_destination_absent(&local_path).await?;

        let mut remote_file = self
            .client
            .open(remote_path.clone())
            .await
            .map_err(|error| remote_error("download", &remote_path, error))?;
        self.begin_partial_file(SftpPath::local(local_path.clone()))?;
        let mut local_file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&local_path)
            .await
            .map_err(|error| {
                self.partial_file = None;
                local_error("create destination file", &local_path, error)
            })?;
        self.confirm_partial_file();

        let transfer_result = async {
            let mut total = 0_u64;
            let mut buffer = vec![0_u8; TRANSFER_CHUNK_BYTES];
            loop {
                let read = remote_file
                    .read(&mut buffer)
                    .await
                    .map_err(|error| remote_error("download", &remote_path, error))?;
                if read == 0 {
                    break;
                }
                local_file
                    .write_all(&buffer[..read])
                    .await
                    .map_err(|error| local_error("write destination file", &local_path, error))?;
                total += read as u64;
            }
            local_file
                .flush()
                .await
                .map_err(|error| local_error("flush destination file", &local_path, error))?;
            Ok(total)
        }
        .await;
        drop(local_file);
        drop(remote_file);

        match transfer_result {
            Ok(byte_count) => {
                self.partial_file = None;
                Ok(SftpCommandOutcome::Downloaded {
                    remote_path,
                    local_path,
                    byte_count,
                })
            }
            Err(error) => Err(self.finish_failed_transfer(error).await),
        }
    }

    async fn put(
        &mut self,
        source: &str,
        destination: Option<&str>,
    ) -> Result<SftpCommandOutcome, SftpSessionError> {
        self.ensure_open()?;
        let local_path = resolve_local_path(&self.local_working_directory, source)?;
        let metadata = fs::metadata(&local_path)
            .await
            .map_err(|error| local_error("read source metadata", &local_path, error))?;
        if !metadata.is_file() {
            return Err(SftpSessionError::LocalOperationFailed {
                operation: "read source file",
                path: display_path(&local_path),
                reason: "source is not a regular file".to_owned(),
            });
        }
        let remote_path = self
            .resolve_remote_transfer_destination(&local_path, destination)
            .await?;

        let mut local_file = OpenOptions::new()
            .read(true)
            .open(&local_path)
            .await
            .map_err(|error| local_error("open source file", &local_path, error))?;

        if self
            .client
            .try_exists(remote_path.clone())
            .await
            .map_err(|error| remote_error("inspect destination path", &remote_path, error))?
        {
            return Err(SftpSessionError::DestinationExists { path: remote_path });
        }

        self.begin_partial_file(SftpPath::remote(remote_path.clone()))?;
        let mut remote_file = self
            .client
            .open_with_flags(
                remote_path.clone(),
                OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
            )
            .await
            .map_err(|error| {
                self.partial_file = None;
                remote_error("upload", &remote_path, error)
            })?;
        self.confirm_partial_file();

        let transfer_result = async {
            let mut total = 0_u64;
            let mut buffer = vec![0_u8; TRANSFER_CHUNK_BYTES];
            loop {
                let read = local_file
                    .read(&mut buffer)
                    .await
                    .map_err(|error| local_error("read source file", &local_path, error))?;
                if read == 0 {
                    break;
                }
                remote_file
                    .write_all(&buffer[..read])
                    .await
                    .map_err(|error| remote_error("upload", &remote_path, error))?;
                total += read as u64;
            }
            remote_file
                .shutdown()
                .await
                .map_err(|error| remote_error("finalize upload", &remote_path, error))?;
            Ok(total)
        }
        .await;
        drop(local_file);
        drop(remote_file);

        match transfer_result {
            Ok(byte_count) => {
                self.partial_file = None;
                Ok(SftpCommandOutcome::Uploaded {
                    local_path,
                    remote_path,
                    byte_count,
                })
            }
            Err(error) => Err(self.finish_failed_transfer(error).await),
        }
    }

    async fn resolve_local_transfer_destination(
        &self,
        remote_source: &str,
        destination: Option<&str>,
    ) -> Result<PathBuf, SftpSessionError> {
        let basename = remote_file_name(remote_source)?.to_owned();
        match destination {
            None => join_remote_name_to_local_directory(
                &self.local_working_directory,
                &basename,
                "resolve download destination",
            ),
            Some(destination) => {
                let mut candidate = resolve_local_path(&self.local_working_directory, destination)?;
                if let Ok(metadata) = fs::metadata(&candidate).await {
                    if metadata.is_dir() {
                        candidate = join_remote_name_to_local_directory(
                            &candidate,
                            &basename,
                            "resolve download destination",
                        )?;
                    }
                }
                Ok(candidate)
            }
        }
    }

    async fn resolve_remote_transfer_destination(
        &self,
        local_source: &Path,
        destination: Option<&str>,
    ) -> Result<String, SftpSessionError> {
        let basename = local_file_name(local_source)?.to_owned();
        match destination {
            None => Ok(join_remote_path(&self.remote_working_directory, &basename)),
            Some(destination) => {
                let mut candidate =
                    resolve_remote_path(&self.remote_working_directory, destination)?;
                if let Ok(metadata) = self.client.metadata(candidate.clone()).await {
                    if metadata.is_dir() {
                        candidate = join_remote_path(&candidate, &basename);
                    }
                }
                Ok(candidate)
            }
        }
    }

    fn ensure_open(&self) -> Result<(), SftpSessionError> {
        if self.closed {
            Err(SftpSessionError::SessionClosed)
        } else {
            Ok(())
        }
    }

    pub(crate) async fn remote_path_metadata_exact(
        &mut self,
        path: &str,
    ) -> Result<Option<SftpPathMetadata>, SftpSessionError> {
        self.ensure_open()?;
        match self.client.metadata(path.to_owned()).await {
            Ok(metadata) => Ok(Some(remote_path_metadata(path, metadata))),
            Err(error) if is_remote_not_found(&error) => Ok(None),
            Err(error) => Err(remote_error("inspect path", path, error)),
        }
    }

    pub(crate) async fn remote_directory_snapshot_exact(
        &mut self,
        path: &str,
    ) -> Result<SftpDirectorySnapshot, SftpSessionError> {
        self.ensure_open()?;
        let metadata = self
            .client
            .metadata(path.to_owned())
            .await
            .map_err(|error| remote_error("list path", path, error))?;
        if !metadata.is_dir() {
            return Err(SftpSessionError::RemotePathNotDirectory {
                path: path.to_owned(),
            });
        }

        let mut entries = self
            .client
            .read_dir(path.to_owned())
            .await
            .map_err(|error| remote_error("list path", path, error))?
            .map(|entry| remote_directory_item(entry.file_name(), entry.path(), entry.metadata()))
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.name.cmp(&right.name));

        Ok(SftpDirectorySnapshot {
            location: SftpLocation::Remote,
            path: SftpPath::remote(path.to_owned()),
            loaded_at: SystemTime::now(),
            entries,
        })
    }

    pub(crate) async fn remote_directory_for_planning(
        &mut self,
        path: &str,
        budget: &SharedPlanningBudget,
    ) -> Result<PlanningQueue<SftpDirectoryItem>, TransferWorkError> {
        self.ensure_open()?;
        let session = self.client.raw_session();
        let handle = session
            .opendir(path)
            .await
            .map_err(|error| remote_error("open directory", path, error))?
            .handle;
        let mut directory = RemotePlanningDirectory {
            session,
            handle: Some(handle),
            runtime: tokio::runtime::Handle::current(),
        };
        let result = async {
            let mut entries = PlanningQueue::new(budget);
            loop {
                let page = match directory
                    .session
                    .readdir(directory.handle.as_deref().expect("directory is open"))
                    .await
                {
                    Ok(page) => page,
                    Err(russh_sftp::client::error::Error::Status(status))
                        if status.status_code == russh_sftp::protocol::StatusCode::Eof =>
                    {
                        break;
                    }
                    Err(error) => return Err(remote_error("read directory", path, error).into()),
                };
                let page_bytes = page.files.iter().fold(
                    page.files
                        .capacity()
                        .saturating_mul(std::mem::size_of::<russh_sftp::protocol::File>()),
                    |bytes, file| {
                        bytes
                            .saturating_add(file.filename.capacity())
                            .saturating_add(file.longname.capacity())
                            .saturating_add(file.attrs.user.as_ref().map_or(0, String::capacity))
                            .saturating_add(file.attrs.group.as_ref().map_or(0, String::capacity))
                    },
                );
                let _page_reservation = budget.reserve(0, page_bytes)?;
                for file in page.files {
                    if matches!(file.filename.as_str(), "." | "..") {
                        continue;
                    }
                    let path_bytes = path
                        .len()
                        .saturating_add(file.filename.len())
                        .saturating_add(1)
                        .saturating_mul(2);
                    let mut reservation = budget.entry(file.filename.capacity(), path_bytes)?;
                    let item_path = join_remote_path(path, &file.filename);
                    let item = remote_directory_item(file.filename, item_path, file.attrs);
                    let actual_path_bytes = path_memory_proxy_bytes(&item.path);
                    if actual_path_bytes > path_bytes {
                        reservation.grow(0, actual_path_bytes - path_bytes)?;
                    }
                    entries.push_back(Budgeted {
                        value: item,
                        reservation,
                    })?;
                }
            }
            entries.sort_by_name();
            Ok(entries)
        }
        .await;
        match directory.close().await {
            Ok(()) => result,
            Err(close_error) => Err(TransferWorkError::DirectoryCloseFailed {
                operation_error: result.err().map(Box::new),
                close_error: remote_error("close directory", path, close_error),
            }),
        }
    }

    pub(crate) async fn create_remote_directory_exact(
        &mut self,
        path: &str,
    ) -> Result<(), SftpSessionError> {
        self.ensure_open()?;
        self.client
            .create_dir(path.to_owned())
            .await
            .map_err(|error| remote_error("create directory", path, error))
    }

    pub(crate) async fn remove_remote_file_exact(
        &mut self,
        path: &str,
    ) -> Result<(), SftpSessionError> {
        self.ensure_open()?;
        self.client
            .remove_file(path.to_owned())
            .await
            .map_err(|error| remote_error("remove file", path, error))
    }

    pub(crate) async fn rename_remote_path_exact(
        &mut self,
        source: &str,
        destination: &str,
    ) -> Result<(), SftpSessionError> {
        self.ensure_open()?;
        self.client
            .rename(source.to_owned(), destination.to_owned())
            .await
            .map_err(|error| SftpSessionError::RemotePairOperationFailed {
                operation: "rename",
                source: source.to_owned(),
                destination: destination.to_owned(),
                reason: error.to_string(),
            })
    }

    pub(crate) async fn upload_local_file_exact<F>(
        &mut self,
        local_path: &Path,
        remote_path: &str,
        on_progress: &mut F,
    ) -> Result<u64, SftpSessionError>
    where
        F: FnMut(u64) -> Result<(), SftpSessionError>,
    {
        self.ensure_open()?;
        let mut local_file = OpenOptions::new()
            .read(true)
            .open(local_path)
            .await
            .map_err(|error| local_error("open source file", local_path, error))?;
        self.begin_partial_file(SftpPath::remote(remote_path.to_owned()))?;
        let mut remote_file = self
            .client
            .open_with_flags(
                remote_path.to_owned(),
                OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
            )
            .await
            .map_err(|error| {
                self.partial_file = None;
                remote_error("upload", remote_path, error)
            })?;
        self.confirm_partial_file();

        let transfer_result = async {
            let mut total = 0_u64;
            let mut buffer = vec![0_u8; TRANSFER_CHUNK_BYTES];
            loop {
                let read = local_file
                    .read(&mut buffer)
                    .await
                    .map_err(|error| local_error("read source file", local_path, error))?;
                if read == 0 {
                    break;
                }
                remote_file
                    .write_all(&buffer[..read])
                    .await
                    .map_err(|error| remote_error("upload", remote_path, error))?;
                total += read as u64;
                on_progress(total)?;
            }
            remote_file
                .shutdown()
                .await
                .map_err(|error| remote_error("finalize upload", remote_path, error))?;
            Ok(total)
        }
        .await;
        drop(local_file);
        drop(remote_file);

        transfer_result
    }

    pub(crate) async fn download_remote_file_exact<F>(
        &mut self,
        remote_path: &str,
        local_path: &Path,
        on_progress: &mut F,
    ) -> Result<u64, SftpSessionError>
    where
        F: FnMut(u64) -> Result<(), SftpSessionError>,
    {
        self.ensure_open()?;
        let mut remote_file = self
            .client
            .open(remote_path.to_owned())
            .await
            .map_err(|error| remote_error("download", remote_path, error))?;
        self.begin_partial_file(SftpPath::local(local_path.to_path_buf()))?;
        let mut local_file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(local_path)
            .await
            .map_err(|error| {
                self.partial_file = None;
                local_error("create destination file", local_path, error)
            })?;
        self.confirm_partial_file();

        let transfer_result = async {
            let mut total = 0_u64;
            let mut buffer = vec![0_u8; TRANSFER_CHUNK_BYTES];
            loop {
                let read = remote_file
                    .read(&mut buffer)
                    .await
                    .map_err(|error| remote_error("download", remote_path, error))?;
                if read == 0 {
                    break;
                }
                local_file
                    .write_all(&buffer[..read])
                    .await
                    .map_err(|error| local_error("write destination file", local_path, error))?;
                total += read as u64;
                on_progress(total)?;
            }
            local_file
                .flush()
                .await
                .map_err(|error| local_error("flush destination file", local_path, error))?;
            Ok(total)
        }
        .await;
        drop(local_file);
        drop(remote_file);

        transfer_result
    }
}

/// Parses one line of text-mode SFTP input.
pub fn parse_sftp_command(line: &str) -> Result<SftpCommand, SftpCommandParseError> {
    let tokens = tokenize_command_line(line)?;
    let (command, arguments) = tokens.split_first().ok_or(SftpCommandParseError::Empty)?;
    if command.starts_with('!') {
        return Err(SftpCommandParseError::UnsupportedFeature {
            feature: "shell escapes",
        });
    }
    if arguments
        .iter()
        .any(|argument| argument.contains('*') || argument.contains('?') || argument.contains('['))
    {
        return Err(SftpCommandParseError::UnsupportedFeature {
            feature: "globbing/wildcard expansion",
        });
    }
    if arguments
        .iter()
        .any(|argument| argument == "-r" || argument == "--recursive")
    {
        return Err(SftpCommandParseError::UnsupportedFeature {
            feature: "recursive transfers",
        });
    }

    match command.to_ascii_lowercase().as_str() {
        "help" => require_no_arguments("help", "help", arguments).map(|()| SftpCommand::Help),
        "pwd" => require_no_arguments("pwd", "pwd", arguments).map(|()| SftpCommand::Pwd),
        "lpwd" => require_no_arguments("lpwd", "lpwd", arguments).map(|()| SftpCommand::Lpwd),
        "cd" => require_exactly_one("cd", "cd <remote-directory>", arguments)
            .map(|path| SftpCommand::Cd { path }),
        "lcd" => require_exactly_one("lcd", "lcd <local-directory>", arguments)
            .map(|path| SftpCommand::Lcd { path }),
        "ls" | "dir" => parse_ls_command(arguments),
        "lls" | "ldir" => parse_lls_command(arguments),
        "mkdir" => require_exactly_one("mkdir", "mkdir <remote-directory>", arguments)
            .map(|path| SftpCommand::Mkdir { path }),
        "rmdir" => require_exactly_one("rmdir", "rmdir <remote-directory>", arguments)
            .map(|path| SftpCommand::Rmdir { path }),
        "rm" => require_exactly_one("rm", "rm <remote-path>", arguments)
            .map(|path| SftpCommand::Rm { path }),
        "rename" => require_exactly_two(
            "rename",
            "rename <old-remote-path> <new-remote-path>",
            arguments,
        )
        .map(|(source, destination)| SftpCommand::Rename {
            source,
            destination,
        }),
        "chmod" => require_exactly_two("chmod", "chmod <octal-mode> <remote-path>", arguments)
            .and_then(|(mode, path)| {
                if mode.is_empty() || !mode.chars().all(|character| matches!(character, '0'..='7'))
                {
                    return Err(SftpCommandParseError::InvalidMode { value: mode });
                }
                u32::from_str_radix(&mode, 8)
                    .map(|mode| SftpCommand::Chmod { mode, path })
                    .map_err(|_| SftpCommandParseError::InvalidMode { value: mode })
            }),
        "get" => parse_transfer_command("get", "get <remote-path> [local-destination]", arguments)
            .map(|(source, destination)| SftpCommand::Get {
                source,
                destination,
            }),
        "put" => parse_transfer_command("put", "put <local-path> [remote-destination]", arguments)
            .map(|(source, destination)| SftpCommand::Put {
                source,
                destination,
            }),
        "quit" | "exit" => {
            require_no_arguments("quit", "quit", arguments).map(|()| SftpCommand::Quit)
        }
        "reget" | "reput" => Err(SftpCommandParseError::UnsupportedFeature {
            feature: "resumable transfers",
        }),
        "symlink" => Err(SftpCommandParseError::UnsupportedFeature {
            feature: "symlink management",
        }),
        "chown" => Err(SftpCommandParseError::UnsupportedFeature {
            feature: "ownership changes",
        }),
        _ => Err(SftpCommandParseError::UnsupportedCommand {
            command: command.clone(),
        }),
    }
}

async fn wait_for_subsystem_acceptance(
    channel: &mut russh::Channel<russh::client::Msg>,
) -> Result<(), SftpSessionError> {
    loop {
        match channel.wait().await {
            Some(russh::ChannelMsg::Success) => return Ok(()),
            Some(
                russh::ChannelMsg::Failure | russh::ChannelMsg::Eof | russh::ChannelMsg::Close,
            )
            | None => return Err(SftpSessionError::SubsystemRejected),
            Some(_) => {}
        }
    }
}

fn require_no_arguments(
    command: &'static str,
    usage: &'static str,
    arguments: &[String],
) -> Result<(), SftpCommandParseError> {
    if arguments.is_empty() {
        Ok(())
    } else {
        Err(SftpCommandParseError::InvalidArguments { command, usage })
    }
}

fn require_exactly_one(
    command: &'static str,
    usage: &'static str,
    arguments: &[String],
) -> Result<String, SftpCommandParseError> {
    match arguments {
        [value] => Ok(value.clone()),
        _ => Err(SftpCommandParseError::InvalidArguments { command, usage }),
    }
}

fn require_exactly_two(
    command: &'static str,
    usage: &'static str,
    arguments: &[String],
) -> Result<(String, String), SftpCommandParseError> {
    match arguments {
        [first, second] => Ok((first.clone(), second.clone())),
        _ => Err(SftpCommandParseError::InvalidArguments { command, usage }),
    }
}

fn parse_transfer_command(
    command: &'static str,
    usage: &'static str,
    arguments: &[String],
) -> Result<(String, Option<String>), SftpCommandParseError> {
    match arguments {
        [source] => Ok((source.clone(), None)),
        [source, destination] => Ok((source.clone(), Some(destination.clone()))),
        _ => Err(SftpCommandParseError::InvalidArguments { command, usage }),
    }
}

fn parse_ls_command(arguments: &[String]) -> Result<SftpCommand, SftpCommandParseError> {
    let mut path = None;
    for argument in arguments {
        if argument.starts_with('-') {
            let options = argument.trim_start_matches('-');
            if options.is_empty() || !options.chars().all(|option| matches!(option, 'a' | 'l')) {
                return Err(SftpCommandParseError::InvalidArguments {
                    command: "ls",
                    usage: "ls [-al] [remote-path]",
                });
            }
        } else if path.replace(argument.clone()).is_some() {
            return Err(SftpCommandParseError::InvalidArguments {
                command: "ls",
                usage: "ls [-al] [remote-path]",
            });
        }
    }
    Ok(SftpCommand::Ls { path })
}

fn parse_lls_command(arguments: &[String]) -> Result<SftpCommand, SftpCommandParseError> {
    for argument in arguments {
        let options = argument.trim_start_matches('-');
        if !argument.starts_with('-')
            || options.is_empty()
            || !options.chars().all(|option| matches!(option, 'a' | 'l'))
        {
            return Err(SftpCommandParseError::InvalidArguments {
                command: "lls",
                usage: "lls [-al]",
            });
        }
    }
    Ok(SftpCommand::Lls)
}

fn tokenize_command_line(line: &str) -> Result<Vec<String>, SftpCommandParseError> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escape_next = false;

    for character in line.trim().chars() {
        if escape_next {
            current.push(character);
            escape_next = false;
            continue;
        }
        match character {
            // Only treat `\` as an escape character *outside* of quotes
            // (e.g. to let an unquoted token contain an escaped space).
            // Windows paths are full of literal backslashes
            // (`C:\Users\name\...`), and users overwhelmingly type them
            // inside double quotes (`lcd "C:\Users\name\Downloads"`); if `\`
            // were still treated as an escape character there, every
            // backslash-letter pair would be silently collapsed to just the
            // letter, corrupting the path (see issue found via the
            // `controlled_openssh_sftp_*` interop tests on Windows).
            '\\' if !in_single_quote && !in_double_quote => escape_next = true,
            '\'' if !in_double_quote => in_single_quote = !in_single_quote,
            '"' if !in_single_quote => in_double_quote = !in_double_quote,
            character if character.is_whitespace() && !in_single_quote && !in_double_quote => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(character),
        }
    }

    if escape_next || in_single_quote || in_double_quote {
        return Err(SftpCommandParseError::UnterminatedQuote);
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    if tokens.is_empty() {
        Err(SftpCommandParseError::Empty)
    } else {
        Ok(tokens)
    }
}

pub(crate) fn resolve_remote_path(base: &str, input: &str) -> Result<String, SftpSessionError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(SftpSessionError::EmptyRemotePath);
    }
    let candidate = if input.starts_with('/') {
        input.to_owned()
    } else if base == "/" {
        format!("/{input}")
    } else {
        format!("{base}/{input}")
    };
    Ok(normalize_remote_path(&candidate))
}

pub(crate) fn normalize_remote_path(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                let _ = components.pop();
            }
            _ => components.push(component),
        }
    }
    if absolute {
        if components.is_empty() {
            "/".to_owned()
        } else {
            format!("/{}", components.join("/"))
        }
    } else if components.is_empty() {
        ".".to_owned()
    } else {
        components.join("/")
    }
}

pub(crate) fn resolve_local_path(base: &Path, input: &str) -> Result<PathBuf, SftpSessionError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(SftpSessionError::EmptyLocalPath);
    }
    let input_path = Path::new(input);
    let home_expansion = local_home_directory()
        .as_deref()
        .and_then(|home| expand_local_home_path(input, home));
    let candidate = if let Some(path) = home_expansion {
        path
    } else if input_path.is_absolute() {
        input_path.to_path_buf()
    } else {
        base.join(input_path)
    };
    Ok(normalize_local_path(candidate))
}

fn local_home_directory() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn expand_local_home_path(input: &str, home: &Path) -> Option<PathBuf> {
    match input {
        "~" => Some(home.to_path_buf()),
        _ => input
            .strip_prefix("~/")
            .or_else(|| input.strip_prefix("~\\"))
            .map(|suffix| home.join(suffix)),
    }
}

pub(crate) fn normalize_local_path(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            // Use the platform's native separator rather than a literal
            // `/`, so the reconstructed path is consistent with the
            // separators used for the `Normal` components pushed below
            // (on Windows `PathBuf::push` always joins with `\`, so
            // forcing `/` here left root-relative paths looking like
            // `C:/Users\name\...` -- a jarring, inconsistent mix).
            Component::RootDir => normalized.push(std::path::MAIN_SEPARATOR.to_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

async fn prepare_local_working_directory(path: PathBuf) -> Result<PathBuf, SftpSessionError> {
    let canonical = fs::canonicalize(&path).await.map_err(|error| {
        SftpSessionError::LocalDirectoryUnavailable {
            path: display_path(&path),
            reason: error.to_string(),
        }
    })?;
    let metadata = fs::metadata(&canonical).await.map_err(|error| {
        SftpSessionError::LocalDirectoryUnavailable {
            path: display_path(&canonical),
            reason: error.to_string(),
        }
    })?;
    if !metadata.is_dir() {
        return Err(SftpSessionError::LocalPathNotDirectory {
            path: display_path(&canonical),
        });
    }
    Ok(canonical)
}

async fn ensure_local_destination_absent(path: &Path) -> Result<(), SftpSessionError> {
    match fs::metadata(path).await {
        Ok(_) => Err(SftpSessionError::DestinationExists {
            path: display_path(path),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(local_error("inspect destination path", path, error)),
    }
}

pub(crate) fn remote_file_name(path: &str) -> Result<&str, SftpSessionError> {
    let trimmed = path.trim_end_matches('/');
    let name = trimmed
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .ok_or_else(|| SftpSessionError::MissingFileName {
            path: path.to_owned(),
        })?;
    if name == "." || name == ".." {
        return Err(SftpSessionError::MissingFileName {
            path: path.to_owned(),
        });
    }
    Ok(name)
}

pub(crate) fn local_file_name(path: &Path) -> Result<&str, SftpSessionError> {
    path.file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| SftpSessionError::MissingFileName {
            path: display_path(path),
        })
}

pub(crate) fn join_path_segment(base: &Path, segment: &str) -> PathBuf {
    normalize_local_path(base.join(segment))
}

pub(crate) fn validate_remote_local_file_name(
    name: &str,
    operation: &'static str,
) -> Result<(), SftpSessionError> {
    // A Unix basename must not acquire path, stream, or device semantics locally.
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    let reserved_device = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
        || (stem.get(..3).is_some_and(|prefix| {
            prefix.eq_ignore_ascii_case("COM") || prefix.eq_ignore_ascii_case("LPT")
        }) && stem.get(3..).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        }));
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.ends_with(['.', ' '])
        || name.chars().any(|character| {
            character <= '\u{1f}'
                || matches!(
                    character,
                    '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*'
                )
        })
        || reserved_device
    {
        return Err(SftpSessionError::LocalOperationFailed {
            operation,
            path: name.to_owned(),
            reason: "remote file name is not safe for a local destination; rename the remote entry or specify a safe local filename for a single-file download".to_owned(),
        });
    }
    Ok(())
}

pub(crate) fn join_remote_name_to_local_directory(
    directory: &Path,
    name: &str,
    operation: &'static str,
) -> Result<PathBuf, SftpSessionError> {
    validate_remote_local_file_name(name, operation)?;
    let directory = normalize_local_path(directory.to_path_buf());
    let candidate = join_path_segment(&directory, name);
    if candidate.parent() == Some(directory.as_path())
        && candidate.file_name() == Some(std::ffi::OsStr::new(name))
    {
        return Ok(candidate);
    }
    Err(SftpSessionError::LocalOperationFailed {
        operation,
        path: display_path(&candidate),
        reason: format!(
            "resolved destination escaped the requested local directory {}",
            display_path(&directory)
        ),
    })
}

pub(crate) fn join_remote_path(base: &str, segment: &str) -> String {
    if base == "/" {
        format!("/{segment}")
    } else {
        format!("{base}/{segment}")
    }
}

pub(crate) fn display_path(path: &Path) -> String {
    path.display().to_string()
}

pub(crate) fn local_error(
    operation: &'static str,
    path: &Path,
    error: impl fmt::Display,
) -> SftpSessionError {
    SftpSessionError::LocalOperationFailed {
        operation,
        path: display_path(path),
        reason: error.to_string(),
    }
}

pub(crate) fn remote_error(
    operation: &'static str,
    path: &str,
    error: impl fmt::Display,
) -> SftpSessionError {
    SftpSessionError::RemoteOperationFailed {
        operation,
        path: path.to_owned(),
        reason: error.to_string(),
    }
}

pub(crate) fn is_remote_not_found(error: &russh_sftp::client::error::Error) -> bool {
    matches!(
        error,
        russh_sftp::client::error::Error::Status(status)
            if status.status_code == russh_sftp::protocol::StatusCode::NoSuchFile
    )
}

pub(crate) fn local_permissions(metadata: &std::fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        Some(metadata.permissions().mode())
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

pub(crate) async fn read_local_path_metadata(
    path: &Path,
) -> Result<Option<SftpPathMetadata>, SftpSessionError> {
    match fs::symlink_metadata(path).await {
        Ok(metadata) => Ok(Some(local_path_metadata(path, &metadata))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(local_error("inspect path", path, error)),
    }
}

async fn open_local_directory(path: &Path) -> Result<(PathBuf, fs::ReadDir), SftpSessionError> {
    let canonical = fs::canonicalize(path).await.map_err(|error| {
        SftpSessionError::LocalDirectoryUnavailable {
            path: display_path(path),
            reason: error.to_string(),
        }
    })?;
    let metadata = fs::metadata(&canonical).await.map_err(|error| {
        SftpSessionError::LocalDirectoryUnavailable {
            path: display_path(&canonical),
            reason: error.to_string(),
        }
    })?;
    if !metadata.is_dir() {
        return Err(SftpSessionError::LocalPathNotDirectory {
            path: display_path(&canonical),
        });
    }

    let directory = fs::read_dir(&canonical)
        .await
        .map_err(|error| local_error("list directory", &canonical, error))?;
    Ok((canonical, directory))
}

pub(crate) async fn read_local_directory_snapshot(
    path: &Path,
) -> Result<SftpDirectorySnapshot, SftpSessionError> {
    let (canonical, mut directory) = open_local_directory(path).await?;
    let mut entries = Vec::new();
    while let Some(entry) = directory
        .next_entry()
        .await
        .map_err(|error| local_error("list directory", &canonical, error))?
    {
        let entry_path = entry.path();
        let metadata = fs::symlink_metadata(&entry_path)
            .await
            .map_err(|error| local_error("inspect path", &entry_path, error))?;
        entries.push(local_directory_item(
            entry.file_name().to_string_lossy().into_owned(),
            &entry_path,
            &metadata,
            LocalSizePolicy::AllEntries,
        ));
    }

    Ok(finish_local_snapshot(canonical, entries))
}

pub(crate) async fn read_local_directory_for_planning(
    path: &Path,
    budget: &SharedPlanningBudget,
) -> Result<PlanningQueue<SftpDirectoryItem>, TransferWorkError> {
    let (_canonical, mut directory) = open_local_directory(path).await?;
    let mut entries = PlanningQueue::new(budget);
    while let Some(entry) = directory
        .next_entry()
        .await
        .map_err(|error| local_error("list directory", path, error))?
    {
        let entry_path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let reservation = budget.entry(name.capacity(), entry_path.capacity())?;
        let metadata = fs::symlink_metadata(&entry_path)
            .await
            .map_err(|error| local_error("inspect path", &entry_path, error))?;
        entries.push_back(Budgeted {
            value: local_directory_item(name, &entry_path, &metadata, LocalSizePolicy::AllEntries),
            reservation,
        })?;
    }
    entries.sort_by_name();
    Ok(entries)
}

/// Synchronous counterpart to [`read_local_directory_snapshot`] for callers
/// without a `tokio` runtime (for example, blocking GUI background threads).
///
/// Unlike the async version, this does not canonicalize `path`: the
/// snapshot's path is the literal path the caller navigated to, and entry
/// sizes are only populated for regular files (directories and other entry
/// types report no size), matching what the caller displays.
pub fn read_local_directory_snapshot_sync(
    path: &Path,
) -> Result<SftpDirectorySnapshot, SftpSessionError> {
    let metadata =
        std::fs::metadata(path).map_err(|error| SftpSessionError::LocalDirectoryUnavailable {
            path: display_path(path),
            reason: error.to_string(),
        })?;
    if !metadata.is_dir() {
        return Err(SftpSessionError::LocalPathNotDirectory {
            path: display_path(path),
        });
    }

    let mut entries = Vec::new();
    for entry in
        std::fs::read_dir(path).map_err(|error| local_error("list directory", path, error))?
    {
        let entry = entry.map_err(|error| local_error("list directory", path, error))?;
        let entry_path = entry.path();
        let entry_metadata = std::fs::symlink_metadata(&entry_path)
            .map_err(|error| local_error("inspect path", &entry_path, error))?;
        entries.push(local_directory_item(
            entry.file_name().to_string_lossy().into_owned(),
            &entry_path,
            &entry_metadata,
            LocalSizePolicy::RegularFilesOnly,
        ));
    }

    Ok(finish_local_snapshot(path.to_path_buf(), entries))
}

/// Which local entries report a size.
///
/// The transfer/session paths report a size for every entry, while the GUI
/// browser reports one only for regular files so that directories render an
/// empty size column rather than their on-disk inode size.
#[derive(Clone, Copy)]
enum LocalSizePolicy {
    AllEntries,
    RegularFilesOnly,
}

fn local_directory_item(
    name: String,
    path: &Path,
    metadata: &std::fs::Metadata,
    size_policy: LocalSizePolicy,
) -> SftpDirectoryItem {
    SftpDirectoryItem {
        name,
        path: SftpPath::local(path.to_path_buf()),
        file_type: local_entry_type(metadata.file_type()),
        size: match size_policy {
            LocalSizePolicy::AllEntries => Some(metadata.len()),
            LocalSizePolicy::RegularFilesOnly => metadata.is_file().then_some(metadata.len()),
        },
        modified_at: metadata.modified().ok(),
        permissions: local_permissions(metadata),
    }
}

/// Sorts entries and assembles the snapshot shared by both the async and
/// synchronous directory readers.
fn finish_local_snapshot(
    path: PathBuf,
    mut entries: Vec<SftpDirectoryItem>,
) -> SftpDirectorySnapshot {
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    SftpDirectorySnapshot {
        location: SftpLocation::Local,
        path: SftpPath::local(path),
        loaded_at: SystemTime::now(),
        entries,
    }
}

fn local_path_metadata(path: &Path, metadata: &std::fs::Metadata) -> SftpPathMetadata {
    SftpPathMetadata {
        path: SftpPath::local(path.to_path_buf()),
        file_type: local_entry_type(metadata.file_type()),
        size: Some(metadata.len()),
        modified_at: metadata.modified().ok(),
        permissions: local_permissions(metadata),
    }
}

fn local_entry_type(file_type: std::fs::FileType) -> SftpEntryType {
    if file_type.is_dir() {
        SftpEntryType::Directory
    } else if file_type.is_symlink() {
        SftpEntryType::Symlink
    } else if file_type.is_file() {
        SftpEntryType::File
    } else {
        SftpEntryType::Other
    }
}

pub(crate) fn remote_directory_item(
    name: String,
    path: String,
    metadata: russh_sftp::protocol::FileAttributes,
) -> SftpDirectoryItem {
    SftpDirectoryItem {
        name,
        path: SftpPath::remote(path),
        file_type: metadata.file_type().into(),
        size: metadata.size,
        modified_at: metadata.modified().ok(),
        permissions: metadata.permissions,
    }
}

pub(crate) fn remote_path_metadata(
    path: &str,
    metadata: russh_sftp::protocol::FileAttributes,
) -> SftpPathMetadata {
    SftpPathMetadata {
        path: SftpPath::remote(path.to_owned()),
        file_type: metadata.file_type().into(),
        size: metadata.size,
        modified_at: metadata.modified().ok(),
        permissions: metadata.permissions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs as stdfs,
        sync::{
            atomic::{AtomicU64, AtomicUsize, Ordering},
            Arc,
        },
    };

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct PagedDirectoryServer {
        pages: usize,
        entries_per_page: usize,
        reads: Arc<AtomicUsize>,
        closes: Arc<AtomicUsize>,
        fail_close: bool,
        blocked_read: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
        close_notification: Arc<tokio::sync::Notify>,
    }

    impl russh_sftp::server::Handler for PagedDirectoryServer {
        type Error = russh_sftp::protocol::StatusCode;

        fn unimplemented(&self) -> Self::Error {
            Self::Error::OpUnsupported
        }

        async fn opendir(
            &mut self,
            id: u32,
            _path: String,
        ) -> Result<russh_sftp::protocol::Handle, Self::Error> {
            Ok(russh_sftp::protocol::Handle {
                id,
                handle: "owned-directory".to_owned(),
            })
        }

        async fn readdir(
            &mut self,
            id: u32,
            _handle: String,
        ) -> Result<russh_sftp::protocol::Name, Self::Error> {
            let page = self.reads.fetch_add(1, Ordering::AcqRel);
            if let Some((started, release)) = self.blocked_read.take() {
                started.notify_one();
                release.notified().await;
            }
            if page >= self.pages {
                return Err(Self::Error::Eof);
            }
            Ok(russh_sftp::protocol::Name {
                id,
                files: (0..self.entries_per_page)
                    .rev()
                    .map(|index| {
                        russh_sftp::protocol::File::dummy(format!("page-{page}-{index}.txt"))
                    })
                    .collect(),
            })
        }

        async fn close(
            &mut self,
            id: u32,
            _handle: String,
        ) -> Result<russh_sftp::protocol::Status, Self::Error> {
            self.closes.fetch_add(1, Ordering::AcqRel);
            self.close_notification.notify_one();
            if self.fail_close {
                return Err(Self::Error::Failure);
            }
            Ok(russh_sftp::protocol::Status {
                id,
                status_code: Self::Error::Ok,
                error_message: String::new(),
                language_tag: String::new(),
            })
        }
    }

    async fn directory_session(
        handler: PagedDirectoryServer,
    ) -> (SftpSession, tokio::task::JoinHandle<()>) {
        let (client, server) = tokio::io::duplex(16_384);
        let server = tokio::spawn(russh_sftp::server::run(server, handler));
        let client = RusshSftpSession::new(client).await.unwrap();
        (
            SftpSession {
                client,
                remote_working_directory: "/".to_owned(),
                local_working_directory: PathBuf::from("."),
                closed: false,
                partial_file: None,
                runtime_keepalive: None,
            },
            server,
        )
    }

    fn planning_budget(items: usize, bytes: usize) -> SharedPlanningBudget {
        SharedPlanningBudget::new(crate::sftp_transfer::TransferPlanningLimits {
            max_items: items,
            max_memory_proxy_bytes: bytes,
        })
    }

    fn paged_server(
        pages: usize,
        entries_per_page: usize,
    ) -> (PagedDirectoryServer, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let reads = Arc::new(AtomicUsize::new(0));
        let closes = Arc::new(AtomicUsize::new(0));
        (
            PagedDirectoryServer {
                pages,
                entries_per_page,
                reads: Arc::clone(&reads),
                closes: Arc::clone(&closes),
                fail_close: false,
                blocked_read: None,
                close_notification: Arc::new(tokio::sync::Notify::new()),
            },
            reads,
            closes,
        )
    }

    #[test]
    fn remote_planning_refuses_before_another_page_and_closes_the_directory() {
        test_runtime().block_on(async {
            for (items, bytes) in [(2, 1024 * 1024), (100, 1)] {
                let budget = planning_budget(items, bytes);
                let (handler, reads, closes) = paged_server(10, 3);
                let (mut session, server) = directory_session(handler).await;
                assert!(matches!(
                    session
                        .remote_directory_for_planning("/source", &budget)
                        .await,
                    Err(TransferWorkError::PlanningLimitExceeded { .. })
                ));
                assert_eq!(reads.load(Ordering::Acquire), 1);
                assert_eq!(closes.load(Ordering::Acquire), 1);
                assert_eq!(budget.usage(), (0, 0));
                session.close().await.unwrap();
                server.abort();
            }
        });
    }

    #[test]
    fn remote_planning_returns_sorted_budgeted_rows_without_recollecting_pages() {
        test_runtime().block_on(async {
            let budget = planning_budget(6, 1024 * 1024);
            let (handler, reads, closes) = paged_server(3, 2);
            let (mut session, server) = directory_session(handler).await;
            let mut entries = session
                .remote_directory_for_planning("/source", &budget)
                .await
                .unwrap();
            assert_eq!(reads.load(Ordering::Acquire), 4);
            assert_eq!(closes.load(Ordering::Acquire), 1);
            assert_eq!(budget.usage().0, 6);
            for page in 0..3 {
                for index in 0..2 {
                    let entry = entries.pop_front().unwrap();
                    assert_eq!(entry.value.name, format!("page-{page}-{index}.txt"));
                    assert_eq!(
                        entry.value.path,
                        SftpPath::remote(format!("/source/page-{page}-{index}.txt"))
                    );
                    drop(entry);
                }
            }
            drop(entries);
            assert_eq!(budget.usage(), (0, 0));
            session.close().await.unwrap();
            server.abort();
        });
    }

    #[test]
    fn remote_planning_reports_close_failure_without_losing_the_planning_error() {
        test_runtime().block_on(async {
            let budget = planning_budget(1, 1024 * 1024);
            let (mut handler, _reads, closes) = paged_server(2, 3);
            handler.fail_close = true;
            let (mut session, server) = directory_session(handler).await;
            let error = match session
                .remote_directory_for_planning("/source", &budget)
                .await
            {
                Ok(_) => panic!("planning and directory close must fail"),
                Err(error) => error,
            };
            assert!(error.to_string().contains("items limit"));
            assert!(error.to_string().contains("remote directory close failed"));
            assert_eq!(closes.load(Ordering::Acquire), 1);
            assert_eq!(budget.usage(), (0, 0));
            session.close().await.unwrap();
            server.abort();
        });
    }

    #[test]
    fn canceled_remote_planning_closes_its_open_handle_without_closing_the_session() {
        test_runtime().block_on(async {
            let budget = planning_budget(10, 1024 * 1024);
            let (mut handler, _reads, closes) = paged_server(1, 1);
            let started = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            handler.blocked_read = Some((Arc::clone(&started), Arc::clone(&release)));
            let closed = Arc::clone(&handler.close_notification);
            let (mut session, server) = directory_session(handler).await;
            let mut enumeration =
                Box::pin(session.remote_directory_for_planning("/source", &budget));
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                tokio::select! {
                    _ = started.notified() => {}
                    _ = &mut enumeration => panic!("enumeration must wait for the controlled read"),
                }
            })
            .await
            .unwrap();
            drop(enumeration);
            release.notify_one();
            tokio::time::timeout(std::time::Duration::from_secs(2), closed.notified())
                .await
                .unwrap();
            assert_eq!(closes.load(Ordering::Acquire), 1);
            assert_eq!(budget.usage(), (0, 0));
            let entries = session
                .remote_directory_for_planning("/another-directory", &budget)
                .await
                .unwrap();
            drop(entries);
            assert_eq!(closes.load(Ordering::Acquire), 2);
            session.close().await.unwrap();
            server.abort();
        });
    }

    #[test]
    fn local_planning_refuses_oversized_enumeration_and_returns_sorted_rows() {
        let root = unique_test_directory("bounded-planning-enumeration");
        create_directory(&root);
        for index in (0..5).rev() {
            stdfs::write(root.join(format!("row-{index}.txt")), b"row").unwrap();
        }
        test_runtime().block_on(async {
            for (items, bytes) in [(2, 1024 * 1024), (100, 1)] {
                let budget = planning_budget(items, bytes);
                assert!(matches!(
                    read_local_directory_for_planning(&root, &budget).await,
                    Err(TransferWorkError::PlanningLimitExceeded { .. })
                ));
                assert_eq!(budget.usage(), (0, 0));
            }
            let budget = planning_budget(5, 1024 * 1024);
            let mut entries = read_local_directory_for_planning(&root, &budget)
                .await
                .unwrap();
            for index in 0..5 {
                assert_eq!(
                    entries.pop_front().unwrap().value.name,
                    format!("row-{index}.txt")
                );
            }
            drop(entries);
            assert_eq!(budget.usage(), (0, 0));
        });
        stdfs::remove_dir_all(root).unwrap();
    }

    struct EmptySftpServer;

    impl russh_sftp::server::Handler for EmptySftpServer {
        type Error = russh_sftp::protocol::StatusCode;

        fn unimplemented(&self) -> Self::Error {
            russh_sftp::protocol::StatusCode::OpUnsupported
        }
    }

    async fn partial_file_session(
        partial_file: PartialFile,
    ) -> (SftpSession, tokio::task::JoinHandle<()>) {
        let (client, server) = tokio::io::duplex(1024);
        let server = tokio::spawn(russh_sftp::server::run(server, EmptySftpServer));
        let client = RusshSftpSession::new(client).await.unwrap();
        (
            SftpSession {
                client,
                remote_working_directory: "/".to_owned(),
                local_working_directory: PathBuf::from("."),
                closed: false,
                partial_file: Some(partial_file),
                runtime_keepalive: None,
            },
            server,
        )
    }

    #[test]
    fn cancellation_cleanup_refuses_an_unconfirmed_destination_without_removing_it() {
        let root = unique_test_directory("unconfirmed-partial");
        create_directory(&root);
        let path = root.join("existing.bin");
        stdfs::write(&path, b"existing output").unwrap();
        test_runtime().block_on(async {
            let (mut session, server) = partial_file_session(PartialFile {
                path: SftpPath::local(path.clone()),
                owned: false,
                commit_destination: None,
            })
            .await;
            assert!(matches!(
                session.cleanup_interrupted_transfer().await,
                Err(SftpSessionError::PartialFileOwnershipUnconfirmed { .. })
            ));
            assert_eq!(stdfs::read(&path).unwrap(), b"existing output");
            server.abort();
        });
    }

    #[test]
    fn cancellation_cleanup_preserves_a_confirmed_local_partial_file() {
        let root = unique_test_directory("confirmed-partial");
        create_directory(&root);
        let path = root.join("partial.bin");
        stdfs::write(&path, b"incomplete output").unwrap();
        test_runtime().block_on(async {
            let (mut session, server) = partial_file_session(PartialFile {
                path: SftpPath::local(path.clone()),
                owned: true,
                commit_destination: None,
            })
            .await;
            assert!(matches!(
                session.cleanup_interrupted_transfer().await,
                Err(SftpSessionError::PartialFileOwnershipUnconfirmed { .. })
            ));
            assert!(session.partial_file.is_none());
            assert_eq!(stdfs::read(&path).unwrap(), b"incomplete output");
            session
                .begin_partial_file(SftpPath::local(root.join("unrelated.bin")))
                .unwrap();
            server.abort();
        });
    }

    #[test]
    fn failed_text_transfer_reports_original_error_and_preserved_output() {
        let root = unique_test_directory("failed-transfer-preserved-output");
        create_directory(&root);
        let path = root.join("partial.bin");
        stdfs::write(&path, b"incomplete output").unwrap();
        test_runtime().block_on(async {
            let (mut session, server) = partial_file_session(PartialFile {
                path: SftpPath::local(path.clone()),
                owned: true,
                commit_destination: None,
            })
            .await;
            let error = session
                .finish_failed_transfer(SftpSessionError::RemoteOperationFailed {
                    operation: "download",
                    path: "/fixture/source.bin".to_owned(),
                    reason: "controlled read failure".to_owned(),
                })
                .await;
            let detail = error.to_string();
            assert!(detail.contains("controlled read failure"));
            assert!(detail.contains("/fixture/source.bin"));
            assert!(detail.contains("file was not removed"));
            assert!(detail.contains("partial.bin"));
            assert_eq!(stdfs::read(&path).unwrap(), b"incomplete output");
            assert!(session.partial_file.is_none());
            server.abort();
        });
    }

    #[test]
    fn cancellation_cleanup_preserves_a_replaced_local_leaf() {
        let root = unique_test_directory("replaced-partial");
        create_directory(&root);
        let path = root.join("partial.bin");
        let original = root.join("original-partial.bin");
        stdfs::write(&path, b"incomplete output").unwrap();
        test_runtime().block_on(async {
            let (mut session, server) = partial_file_session(PartialFile {
                path: SftpPath::local(path.clone()),
                owned: true,
                commit_destination: None,
            })
            .await;
            stdfs::rename(&path, &original).unwrap();
            stdfs::write(&path, b"unrelated replacement").unwrap();
            let result = session.cleanup_interrupted_transfer().await;
            assert_eq!(stdfs::read(&path).unwrap(), b"unrelated replacement");
            assert_eq!(stdfs::read(&original).unwrap(), b"incomplete output");
            assert!(matches!(
                result,
                Err(SftpSessionError::PartialFileOwnershipUnconfirmed { .. })
            ));
            assert!(session.partial_file.is_none());
            server.abort();
        });
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_cleanup_preserves_files_after_ancestor_symlink_replacement() {
        let root = unique_test_directory("redirected-partial");
        create_directory(&root);
        let selected = root.join("selected");
        let original = root.join("original");
        let private = root.join("private");
        stdfs::create_dir(&selected).unwrap();
        stdfs::create_dir(&private).unwrap();
        let path = selected.join("report.bin");
        stdfs::write(&path, b"incomplete output").unwrap();
        stdfs::write(private.join("report.bin"), b"unrelated private output").unwrap();
        test_runtime().block_on(async {
            let (mut session, server) = partial_file_session(PartialFile {
                path: SftpPath::local(path),
                owned: true,
                commit_destination: None,
            })
            .await;
            stdfs::rename(&selected, &original).unwrap();
            std::os::unix::fs::symlink(&private, &selected).unwrap();
            let result = session.cleanup_interrupted_transfer().await;
            assert_eq!(
                stdfs::read(private.join("report.bin")).unwrap(),
                b"unrelated private output"
            );
            assert_eq!(
                stdfs::read(original.join("report.bin")).unwrap(),
                b"incomplete output"
            );
            assert!(matches!(
                result,
                Err(SftpSessionError::PartialFileOwnershipUnconfirmed { .. })
            ));
            server.abort();
        });
    }

    #[test]
    fn cancellation_cleanup_preserves_a_confirmed_remote_partial_file() {
        test_runtime().block_on(async {
            let (mut session, server) = partial_file_session(PartialFile {
                path: SftpPath::remote("/fixture/partial.bin"),
                owned: true,
                commit_destination: None,
            })
            .await;
            assert!(matches!(
                session.cleanup_interrupted_transfer().await,
                Err(SftpSessionError::PartialFileOwnershipUnconfirmed { .. })
            ));
            assert!(session.partial_file.is_none());
            session
                .begin_partial_file(SftpPath::remote("/fixture/unrelated.bin"))
                .unwrap();
            server.abort();
        });
    }

    #[test]
    fn cancellation_cleanup_preserves_a_partial_during_an_unconfirmed_replace_commit() {
        let root = unique_test_directory("unconfirmed-commit");
        create_directory(&root);
        let temporary = root.join("partial.bin");
        let destination = root.join("destination.bin");
        stdfs::write(&temporary, b"replacement output").unwrap();
        stdfs::write(&destination, b"existing destination").unwrap();
        test_runtime().block_on(async {
            let path = SftpPath::local(temporary.clone());
            let (mut session, server) = partial_file_session(PartialFile {
                path: path.clone(),
                owned: true,
                commit_destination: None,
            })
            .await;
            session.protect_partial_commit(&path, &SftpPath::local(destination.clone()));
            assert!(matches!(
                session.cleanup_interrupted_transfer().await,
                Err(SftpSessionError::TransferCommitInterrupted { .. })
            ));
            assert_eq!(stdfs::read(&temporary).unwrap(), b"replacement output");
            assert_eq!(stdfs::read(&destination).unwrap(), b"existing destination");
            assert!(session.partial_file.is_none());
            session
                .begin_partial_file(SftpPath::local(root.join("unrelated.bin")))
                .unwrap();
            server.abort();
        });
    }

    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("could not build tokio runtime for SFTP tests")
    }

    fn unique_test_directory(label: &str) -> PathBuf {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-artifacts/festerm-ssh-sftp");
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        root.join(format!("{label}-{}-{id}", std::process::id()))
    }

    fn create_directory(path: &Path) {
        if path.exists() {
            stdfs::remove_dir_all(path).expect("could not clear pre-existing test directory");
        }
        stdfs::create_dir_all(path).expect("could not create test directory");
    }

    #[test]
    fn command_parser_accepts_supported_commands() {
        assert_eq!(parse_sftp_command("help"), Ok(SftpCommand::Help));
        assert_eq!(parse_sftp_command("pwd"), Ok(SftpCommand::Pwd));
        assert_eq!(parse_sftp_command("lpwd"), Ok(SftpCommand::Lpwd));
        assert_eq!(
            parse_sftp_command("cd ../remote"),
            Ok(SftpCommand::Cd {
                path: "../remote".to_owned()
            })
        );
        assert_eq!(
            parse_sftp_command("lcd \"folder with spaces\""),
            Ok(SftpCommand::Lcd {
                path: "folder with spaces".to_owned()
            })
        );
        // Windows paths are full of literal backslashes and are typically
        // typed inside double quotes; those backslashes must survive
        // tokenization unchanged instead of being treated as shell-style
        // escape characters (regression test for the interop failure where
        // "C:\Users\fes\Downloads" was silently mangled to "C:UsersfesDownloads").
        assert_eq!(
            parse_sftp_command("lcd \"C:\\Users\\fes\\Downloads\""),
            Ok(SftpCommand::Lcd {
                path: "C:\\Users\\fes\\Downloads".to_owned()
            })
        );
        assert_eq!(parse_sftp_command("ls"), Ok(SftpCommand::Ls { path: None }));
        assert_eq!(
            parse_sftp_command("ls -al"),
            Ok(SftpCommand::Ls { path: None })
        );
        assert_eq!(
            parse_sftp_command("ls -la ./child"),
            Ok(SftpCommand::Ls {
                path: Some("./child".to_owned())
            })
        );
        assert_eq!(
            parse_sftp_command("dir -al ./child"),
            Ok(SftpCommand::Ls {
                path: Some("./child".to_owned())
            })
        );
        assert_eq!(parse_sftp_command("lls"), Ok(SftpCommand::Lls));
        assert_eq!(parse_sftp_command("lls -al"), Ok(SftpCommand::Lls));
        assert_eq!(parse_sftp_command("ldir"), Ok(SftpCommand::Lls));
        assert_eq!(parse_sftp_command("ldir -la"), Ok(SftpCommand::Lls));
        assert_eq!(
            parse_sftp_command("ls ./child"),
            Ok(SftpCommand::Ls {
                path: Some("./child".to_owned())
            })
        );
        assert_eq!(
            parse_sftp_command("rename old new"),
            Ok(SftpCommand::Rename {
                source: "old".to_owned(),
                destination: "new".to_owned()
            })
        );
        assert_eq!(
            parse_sftp_command("chmod 755 ./script"),
            Ok(SftpCommand::Chmod {
                mode: 0o755,
                path: "./script".to_owned()
            })
        );
        assert_eq!(
            parse_sftp_command("get remote.txt"),
            Ok(SftpCommand::Get {
                source: "remote.txt".to_owned(),
                destination: None
            })
        );
        assert_eq!(
            parse_sftp_command("put ./local.txt /remote/out.txt"),
            Ok(SftpCommand::Put {
                source: "./local.txt".to_owned(),
                destination: Some("/remote/out.txt".to_owned())
            })
        );
        assert_eq!(parse_sftp_command("exit"), Ok(SftpCommand::Quit));
    }

    #[test]
    fn command_parser_rejects_unsupported_and_invalid_input() {
        assert_eq!(parse_sftp_command("   "), Err(SftpCommandParseError::Empty));
        assert_eq!(
            parse_sftp_command("!ls"),
            Err(SftpCommandParseError::UnsupportedFeature {
                feature: "shell escapes"
            })
        );
        assert_eq!(
            parse_sftp_command("ls *.txt"),
            Err(SftpCommandParseError::UnsupportedFeature {
                feature: "globbing/wildcard expansion"
            })
        );
        assert_eq!(
            parse_sftp_command("get -r remote.txt"),
            Err(SftpCommandParseError::UnsupportedFeature {
                feature: "recursive transfers"
            })
        );
        assert_eq!(
            parse_sftp_command("symlink a b"),
            Err(SftpCommandParseError::UnsupportedFeature {
                feature: "symlink management"
            })
        );
        assert_eq!(
            parse_sftp_command("chown 1000 file"),
            Err(SftpCommandParseError::UnsupportedFeature {
                feature: "ownership changes"
            })
        );
        assert_eq!(
            parse_sftp_command("reget file"),
            Err(SftpCommandParseError::UnsupportedFeature {
                feature: "resumable transfers"
            })
        );
        assert_eq!(
            parse_sftp_command("chmod 88 file"),
            Err(SftpCommandParseError::InvalidMode {
                value: "88".to_owned()
            })
        );
        assert_eq!(
            parse_sftp_command("rename only-one"),
            Err(SftpCommandParseError::InvalidArguments {
                command: "rename",
                usage: "rename <old-remote-path> <new-remote-path>"
            })
        );
        assert_eq!(
            parse_sftp_command("mystery"),
            Err(SftpCommandParseError::UnsupportedCommand {
                command: "mystery".to_owned()
            })
        );
        assert_eq!(
            parse_sftp_command("lcd \"unterminated"),
            Err(SftpCommandParseError::UnterminatedQuote)
        );
    }

    #[test]
    fn remote_path_resolution_normalizes_relative_segments() {
        assert_eq!(
            resolve_remote_path("/home/test", "docs/../logs").expect("remote path resolves"),
            "/home/test/logs"
        );
        assert_eq!(
            resolve_remote_path("/home/test", "../../etc").expect("remote path resolves"),
            "/etc"
        );
        assert_eq!(
            resolve_remote_path("/home/test", "/srv/./files/../data")
                .expect("remote path resolves"),
            "/srv/data"
        );
    }

    #[test]
    fn local_home_abbreviations_expand_without_treating_tilde_as_a_literal_directory() {
        let home = Path::new("/Users/fes");
        assert_eq!(expand_local_home_path("~", home), Some(home.to_path_buf()));
        assert_eq!(
            expand_local_home_path("~/Downloads", home),
            Some(PathBuf::from("/Users/fes/Downloads"))
        );
        assert_eq!(
            expand_local_home_path("~\\Downloads", home),
            Some(PathBuf::from("/Users/fes/Downloads"))
        );
        assert_eq!(expand_local_home_path("~other", home), None);
    }

    #[test]
    fn local_path_resolution_allows_explicit_parent_navigation() {
        let base = PathBuf::from("/Users/fes/src/fesTerm/crates/festerm-ssh");
        assert_eq!(
            resolve_local_path(&base, "../docs").expect("local path resolves"),
            PathBuf::from("/Users/fes/src/fesTerm/crates/docs")
        );
        assert_eq!(
            resolve_local_path(&base, "./src/../tests").expect("local path resolves"),
            PathBuf::from("/Users/fes/src/fesTerm/crates/festerm-ssh/tests")
        );
    }

    #[test]
    fn transfer_destination_derivation_keeps_only_the_source_basename() {
        let base = PathBuf::from("/workspace/downloads");
        assert_eq!(
            join_path_segment(
                &base,
                remote_file_name("/srv/files/report.txt").expect("basename")
            ),
            PathBuf::from("/workspace/downloads/report.txt")
        );
        assert_eq!(
            remote_file_name("/srv/files/../etc/passwd").expect("basename"),
            "passwd"
        );
        assert_eq!(
            remote_file_name("/").expect_err("root has no basename"),
            SftpSessionError::MissingFileName {
                path: "/".to_owned()
            }
        );
        assert_eq!(
            remote_file_name("/srv/.").expect_err("dot is not a basename"),
            SftpSessionError::MissingFileName {
                path: "/srv/.".to_owned()
            }
        );
    }

    #[test]
    fn local_destination_absence_check_refuses_overwrite() {
        let root = unique_test_directory("overwrite-refusal");
        create_directory(&root);
        let existing = root.join("existing.txt");
        stdfs::write(&existing, b"original").expect("could not create destination fixture");

        test_runtime().block_on(async {
            assert_eq!(
                ensure_local_destination_absent(&existing)
                    .await
                    .expect_err("existing file must be refused"),
                SftpSessionError::DestinationExists {
                    path: display_path(&existing)
                }
            );
        });

        stdfs::remove_dir_all(&root).expect("could not clean test directory");
    }

    #[test]
    fn prepare_local_working_directory_requires_an_existing_directory() {
        let root = unique_test_directory("lcd-boundaries");
        let child = root.join("child");
        let nested = child.join("nested");
        create_directory(&nested);
        let sibling = root.join("sibling");
        stdfs::create_dir_all(&sibling).expect("could not create sibling directory");

        test_runtime().block_on(async {
            let resolved = prepare_local_working_directory(
                resolve_local_path(&nested, "../../sibling").expect("path resolves"),
            )
            .await
            .expect("lcd-style parent traversal is allowed when it lands on a real directory");
            assert_eq!(
                resolved,
                stdfs::canonicalize(&sibling).expect("canonical sibling directory")
            );

            let missing = prepare_local_working_directory(
                resolve_local_path(&nested, "../../missing").expect("path resolves"),
            )
            .await
            .expect_err("missing target directory must be rejected");
            assert!(matches!(
                missing,
                SftpSessionError::LocalDirectoryUnavailable { .. }
            ));
        });

        stdfs::remove_dir_all(&root).expect("could not clean test directory");
    }
}
