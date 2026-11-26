//! Explicitly rooted filesystem service; independent of terminal attachment.
use crate::{
    file_protocol::{self as rpc, ErrorCode, Operation, Result as Reply},
    protocol::{invalid, Target},
    wire::{read_client_frame, write_frame, DEADLINE},
};
use cap_fs_ext::DirExt;
use cap_std::fs::{Dir, ReadDir};
use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Component, Path},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{OwnedSemaphorePermit, Semaphore},
};
use tokio_util::sync::CancellationToken;

mod reviewed_delete;
use reviewed_delete::ReviewedEntry;

const RESERVED: &str = ".festerm-transfer-";

pub struct Filesystem {
    root: Dir,
    start_in: String,
    display_root: Option<rpc::DisplayRoot>,
    write: bool,
    delete: bool,
    slots: Arc<Semaphore>,
}

fn trusted(directory: &Dir) -> io::Result<()> {
    let file = directory.try_clone()?.into_std_file();
    #[cfg(unix)]
    return festerm_unix_security::trusted_private_storage_ancestor(&file);
    #[cfg(windows)]
    return festerm_windows_security::trusted_private_storage_ancestor(&file);
}

impl Filesystem {
    pub fn open(policy: &rpc::Policy) -> io::Result<Arc<Self>> {
        policy.validate()?;
        let mut components = policy.root.components();
        #[cfg(unix)]
        let mut directory = {
            if components.next() != Some(Component::RootDir) {
                return Err(invalid());
            }
            Dir::open_ambient_dir("/", cap_std::ambient_authority())?
        };
        #[cfg(windows)]
        let mut directory = {
            let Some(Component::Prefix(prefix)) = components.next() else {
                return Err(invalid());
            };
            if !matches!(prefix.kind(), std::path::Prefix::Disk(_))
                || components.next() != Some(Component::RootDir)
            {
                return Err(invalid());
            }
            let mut path = prefix.as_os_str().to_os_string();
            path.push("\\");
            let path = std::path::PathBuf::from(path);
            festerm_windows_security::require_local_disk_root(&path)?;
            Dir::from_std_file(
                festerm_windows_security::lock_directory_path_without_delete_sharing(&path)?,
            )
        };
        for component in components {
            trusted(&directory)?;
            let Component::Normal(name) = component else {
                return Err(invalid());
            };
            directory = directory.open_dir_nofollow(name)?;
        }
        trusted(&directory)?;
        let filesystem = Arc::new(Self {
            root: directory,
            start_in: policy.initial_directory().to_owned(),
            display_root: policy
                .root
                .to_str()
                .and_then(|path| path.to_owned().try_into().ok()),
            write: policy.write,
            delete: policy.delete,
            slots: Arc::new(Semaphore::new(2)),
        });
        Ok(filesystem)
    }

    /// Owner preflight and fresh Files opening only; an unavailable initial
    /// directory must never prevent preparation of the allowed root for HOST.
    pub fn validate_start_directory(&self) -> io::Result<()> {
        self.directory(&self.start_in).map(|_| ())
    }

    pub fn capabilities(&self) -> rpc::Capabilities {
        rpc::Capabilities {
            version: rpc::VERSION,
            write: self.write,
            chunk_limit: rpc::CHUNK_LIMIT,
            page_limit: rpc::PAGE_LIMIT,
            handle_limit: rpc::HANDLE_LIMIT,
        }
    }

    fn operation_capabilities(&self) -> rpc::OperationCapabilities {
        let base = self.capabilities();
        rpc::OperationCapabilities {
            version: rpc::HOST_REVIEWED_VERSION,
            write: base.write,
            operations: {
                let mut operations = vec!["start_directory".into()];
                if self.write && self.delete {
                    operations.push("delete".into());
                }
                if self.display_root.is_some() {
                    operations.push("display_root".into());
                }
                operations
            },
            chunk_limit: base.chunk_limit,
            page_limit: base.page_limit,
            handle_limit: base.handle_limit,
        }
    }

    pub(crate) fn directory(&self, path: &str) -> io::Result<Dir> {
        let names = names(path)?;
        let mut directory = self.root.try_clone()?;
        for name in names {
            directory = directory.open_dir_nofollow(name)?;
            trusted(&directory)?;
        }
        Ok(directory)
    }

    pub(crate) fn parent(&self, path: &str) -> io::Result<(Dir, String)> {
        let mut names = names(path)?;
        let name = names.pop().ok_or_else(invalid)?.to_owned();
        let parent = format!("/{}", names.join("/"));
        Ok((self.directory(&parent)?, name))
    }
}

fn names(path: &str) -> io::Result<Vec<&str>> {
    if path == "/" {
        return Ok(Vec::new());
    }
    if path.len() > 4096 || !path.starts_with('/') {
        return Err(invalid());
    }
    let names: Vec<_> = path[1..].split('/').collect();
    if names.iter().any(|name| !valid_name(name)) {
        return Err(invalid());
    }
    Ok(names)
}

pub(crate) fn valid_name(name: &str) -> bool {
    rpc::valid_name(name)
}

pub(crate) fn open_file(parent: &Dir, name: &str) -> io::Result<File> {
    let parent = parent.try_clone()?.into_std_file();
    #[cfg(unix)]
    return festerm_unix_security::open_file_nofollow(&parent, Path::new(name));
    #[cfg(windows)]
    return festerm_windows_security::open_file_no_reparse(&parent, Path::new(name));
}

pub(crate) fn open_observation_file(parent: &Dir, name: &str) -> io::Result<File> {
    let parent = parent.try_clone()?.into_std_file();
    #[cfg(unix)]
    return festerm_unix_security::open_file_nofollow(&parent, Path::new(name));
    #[cfg(windows)]
    return festerm_windows_security::open_file_no_reparse(&parent, Path::new(name));
}

fn private_file(parent: &Dir, name: &str) -> io::Result<File> {
    let parent = parent.try_clone()?.into_std_file();
    #[cfg(unix)]
    return festerm_unix_security::open_private_file(&parent, Path::new(name), true);
    #[cfg(windows)]
    return festerm_windows_security::create_current_user_only_file(&parent, Path::new(name));
}

fn publish(parent: &Dir, source: &str, file: &File, target: &str) -> io::Result<()> {
    let parent = parent.try_clone()?.into_std_file();
    #[cfg(unix)]
    {
        let _ = file;
        festerm_unix_security::rename_noreplace(
            &parent,
            Path::new(source),
            &parent,
            Path::new(target),
        )
    }
    #[cfg(windows)]
    {
        let _ = source;
        festerm_windows_security::rename_file_noreplace(file, &parent, Path::new(target))
    }
}

fn remove(parent: &Dir, name: &str, file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        let _ = file;
        parent.remove_file(name)
    }
    #[cfg(windows)]
    {
        let _ = (parent, name);
        festerm_windows_security::remove_private_staging_file(file)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    length: u64,
    modified: SystemTime,
    changed: i128,
}

fn stamp(file: &File) -> io::Result<Stamp> {
    stamp_kind(file, false)
}
fn stamp_kind(file: &File, directory: bool) -> io::Result<Stamp> {
    let metadata = file.metadata()?;
    if if directory {
        !metadata.is_dir()
    } else {
        !metadata.is_file()
    } {
        return Err(invalid());
    }
    #[cfg(unix)]
    let changed = {
        use std::os::unix::fs::MetadataExt;
        i128::from(metadata.ctime()) * 1_000_000_000 + i128::from(metadata.ctime_nsec())
    };
    #[cfg(windows)]
    let changed = i128::from(festerm_windows_security::file_change_time(file)?);
    Ok(Stamp {
        length: if directory { 0 } else { metadata.len() },
        modified: metadata.modified()?,
        changed,
    })
}

fn same(left: &File, right: &File) -> io::Result<bool> {
    #[cfg(windows)]
    return festerm_windows_security::same_file_identity(left, right);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let left = left.metadata()?;
        let right = right.metadata()?;
        Ok(left.dev() == right.dev() && left.ino() == right.ino())
    }
}

fn metadata(file: &File) -> io::Result<rpc::Metadata> {
    let value = file.metadata()?;
    Ok(rpc::Metadata {
        kind: if value.is_file() {
            rpc::Kind::File
        } else if value.is_dir() {
            rpc::Kind::Directory
        } else {
            return Err(invalid());
        },
        size: if value.is_file() { value.len() } else { 0 }.to_string(),
        modified_ms: value
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|time| time.as_millis().to_string()),
    })
}

struct Expected {
    file: File,
    stamp: Stamp,
}
struct Upload {
    parent: Dir,
    path: String,
    target: String,
    temporary: String,
    file: File,
    size: u64,
    written: u64,
    expected: Option<Expected>,
    published: bool,
}

impl Drop for Upload {
    fn drop(&mut self) {
        if !self.published {
            let _ = remove(&self.parent, &self.temporary, &self.file);
        }
    }
}

enum Handle {
    EntryReview(ReviewedEntry),
    Read {
        path: String,
        file: File,
        stamp: Stamp,
    },
    Directory(Box<ReadDir>),
    Upload(Upload),
}

#[derive(Debug)]
struct Failure {
    code: ErrorCode,
    recovery: Option<String>,
}
impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self {
            code: match error.kind() {
                io::ErrorKind::NotFound => ErrorCode::NotFound,
                io::ErrorKind::AlreadyExists => ErrorCode::Exists,
                io::ErrorKind::PermissionDenied => ErrorCode::Denied,
                io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => ErrorCode::Invalid,
                io::ErrorKind::WouldBlock => ErrorCode::Busy,
                _ => ErrorCode::Io,
            },
            recovery: None,
        }
    }
}
impl From<ErrorCode> for Failure {
    fn from(code: ErrorCode) -> Self {
        Self {
            code,
            recovery: None,
        }
    }
}
type Outcome<T> = std::result::Result<T, Failure>;

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum AdmissionStage {
    Request,
    Disclosure,
    Publication,
}
pub(crate) type Admission = Arc<dyn Fn(AdmissionStage) -> io::Result<()> + Send + Sync>;

// This wrapper belongs to one native job, including its queue time. Only a
// fresh protected-policy read may wait; the filesystem operation is never replayed.
fn scoped_admission(
    admission: Admission,
    deadline: tokio::time::Instant,
    cancel: CancellationToken,
) -> Admission {
    Arc::new(move |stage| {
        let mut busy = None;
        loop {
            if cancel.is_cancelled() {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            let Some(_) = deadline
                .checked_duration_since(tokio::time::Instant::now())
                .filter(|remaining| !remaining.is_zero())
            else {
                return Err(busy.unwrap_or_else(|| io::ErrorKind::TimedOut.into()));
            };
            match admission(stage) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    busy = Some(error);
                    // The failed callback has released its lease before waiting.
                    if !cancel.is_cancelled() {
                        if let Some(remaining) =
                            deadline.checked_duration_since(tokio::time::Instant::now())
                        {
                            std::thread::sleep(remaining.min(std::time::Duration::from_millis(25)));
                        }
                    }
                }
                Err(error) => return Err(error),
                Ok(()) => {
                    if cancel.is_cancelled() {
                        return Err(io::ErrorKind::PermissionDenied.into());
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Err(busy.unwrap_or_else(|| io::ErrorKind::TimedOut.into()));
                    }
                    return Ok(());
                }
            }
        }
    })
}

struct Session {
    reviewed_operations: bool,
    #[cfg(test)]
    admission: Admission,
    filesystem: Arc<Filesystem>,
    handles: HashMap<u32, Handle>,
    next: u32,
    _permit: Arc<OwnedSemaphorePermit>,
}

impl Session {
    fn insert(&mut self, handle: Handle) -> Outcome<u32> {
        if self.handles.len() >= rpc::HANDLE_LIMIT {
            return Err(ErrorCode::Busy.into());
        }
        let id = self.next.checked_add(1).ok_or(ErrorCode::Busy)?;
        self.next = id;
        self.handles.insert(id, handle);
        Ok(id)
    }

    #[cfg(test)]
    fn execute(
        &mut self,
        operation: Operation,
        data: Vec<u8>,
        cancel: &CancellationToken,
    ) -> Outcome<(Reply, Option<Vec<u8>>)> {
        self.execute_admitted(operation, data, cancel, &self.admission.clone())
    }

    fn execute_admitted(
        &mut self,
        operation: Operation,
        data: Vec<u8>,
        cancel: &CancellationToken,
        admission: &Admission,
    ) -> Outcome<(Reply, Option<Vec<u8>>)> {
        if cancel.is_cancelled() {
            return Err(ErrorCode::Denied.into());
        }
        let reply = match operation {
            Operation::StartDirectory {} => {
                if !self.reviewed_operations {
                    return Err(ErrorCode::Denied.into());
                }
                self.filesystem.validate_start_directory()?;
                Reply::StartDirectory {
                    path: self
                        .filesystem
                        .start_in
                        .clone()
                        .try_into()
                        .map_err(|_| ErrorCode::Invalid)?,
                }
            }
            Operation::DisplayRoot {} => {
                if !self.reviewed_operations {
                    return Err(ErrorCode::Denied.into());
                }
                Reply::DisplayRoot {
                    root: self
                        .filesystem
                        .display_root
                        .clone()
                        .ok_or(ErrorCode::Denied)?,
                }
            }

            Operation::ReviewEntry {
                path,
                purpose: rpc::ReviewPurpose::Delete,
            } => {
                if !self.reviewed_operations || !self.filesystem.delete || !self.filesystem.write {
                    return Err(ErrorCode::Denied.into());
                }
                // Session is exclusively held by this native job: reserve the
                // existing slot before opening any retained review objects.
                if self.handles.len() >= rpc::HANDLE_LIMIT {
                    return Err(ErrorCode::Busy.into());
                }
                let review = reviewed_delete::review(&self.filesystem, &path)?;
                let info = metadata(&review.file)?;
                let handle = self.insert(Handle::EntryReview(review))?;
                Reply::EntryReview {
                    handle,
                    metadata: info,
                }
            }
            Operation::Delete { review } => {
                if !self.reviewed_operations || !self.filesystem.delete || !self.filesystem.write {
                    return Err(ErrorCode::Denied.into());
                }
                if !matches!(self.handles.get(&review), Some(Handle::EntryReview(_))) {
                    return Err(ErrorCode::Invalid.into());
                }
                let Some(Handle::EntryReview(entry)) = self.handles.remove(&review) else {
                    unreachable!()
                };
                reviewed_delete::remove(entry, admission, cancel)?;
                Reply::Deleted
            }
            Operation::Ping {} => Reply::Pong,
            Operation::Close { handle } => {
                self.handles.remove(&handle).ok_or(ErrorCode::Invalid)?;
                Reply::Closed
            }
            Operation::Stat { path } => {
                let file = match self.filesystem.directory(&path) {
                    Ok(directory) => directory.into_std_file(),
                    Err(_) => {
                        let (parent, name) = self.filesystem.parent(&path)?;
                        open_file(&parent, &name)?
                    }
                };
                Reply::Stat {
                    metadata: metadata(&file)?,
                }
            }
            Operation::OpenDirectory { path } => {
                let directory = self.filesystem.directory(&path)?;
                Reply::Directory {
                    handle: self.insert(Handle::Directory(Box::new(directory.entries()?)))?,
                }
            }
            Operation::ReadDirectory { handle, limit } => {
                if limit == 0 || limit > rpc::PAGE_LIMIT {
                    return Err(ErrorCode::Invalid.into());
                }
                let Some(Handle::Directory(iterator)) = self.handles.get_mut(&handle) else {
                    return Err(ErrorCode::Invalid.into());
                };
                let mut entries = Vec::new();
                let (mut done, mut skipped) = (false, 0);
                for _ in 0..rpc::PAGE_LIMIT * 4 {
                    if entries.len() == usize::from(limit) {
                        break;
                    }
                    let Some(entry) = iterator.next() else {
                        done = true;
                        break;
                    };
                    let entry = entry?;
                    let name = entry.file_name();
                    let Some(name) = name.to_str().filter(|name| valid_name(name)) else {
                        skipped += 1;
                        continue;
                    };
                    let kind = entry.file_type()?;
                    entries.push(rpc::Entry {
                        name: name.into(),
                        kind: if kind.is_symlink() {
                            rpc::Kind::Unsupported
                        } else if kind.is_dir() {
                            rpc::Kind::Directory
                        } else if kind.is_file() {
                            rpc::Kind::File
                        } else {
                            rpc::Kind::Unsupported
                        },
                    });
                }
                Reply::Entries {
                    entries,
                    done,
                    skipped,
                }
            }
            Operation::OpenRead { path } => {
                let (parent, name) = self.filesystem.parent(&path)?;
                let file = open_file(&parent, &name)?;
                let value = metadata(&file)?;
                let captured = stamp(&file)?;
                let handle = self.insert(Handle::Read {
                    path,
                    file,
                    stamp: captured,
                })?;
                Reply::File {
                    handle,
                    metadata: value,
                }
            }
            Operation::Read {
                handle,
                offset,
                length,
            } => {
                let offset = rpc::decimal(&offset).ok_or(ErrorCode::Invalid)?;
                if length == 0 || length as usize > rpc::CHUNK_LIMIT {
                    return Err(ErrorCode::Invalid.into());
                }
                let Some(Handle::Read {
                    file,
                    stamp: expected,
                    ..
                }) = self.handles.get_mut(&handle)
                else {
                    return Err(ErrorCode::Invalid.into());
                };
                if stamp(file)? != *expected || offset > expected.length {
                    return Err(ErrorCode::Conflict.into());
                }
                file.seek(SeekFrom::Start(offset))?;
                let mut bytes = vec![0; (u64::from(length).min(expected.length - offset)) as usize];
                file.read_exact(&mut bytes)?;
                if stamp(file)? != *expected {
                    return Err(ErrorCode::Conflict.into());
                }
                let next = offset + bytes.len() as u64;
                return Ok((
                    Reply::Read {
                        offset: offset.to_string(),
                        length: bytes.len() as u32,
                        eof: next == expected.length,
                    },
                    Some(bytes),
                ));
            }
            Operation::Mkdir { path } => {
                if !self.filesystem.write {
                    return Err(ErrorCode::Denied.into());
                }
                let (parent, name) = self.filesystem.parent(&path)?;
                let parent = parent.into_std_file();
                #[cfg(unix)]
                festerm_unix_security::create_private_directory(&parent, Path::new(&name))?;
                #[cfg(windows)]
                festerm_windows_security::create_current_user_only_directory(
                    &parent,
                    Path::new(&name),
                )?;
                Reply::Created
            }
            Operation::BeginUpload {
                path,
                size,
                expected,
            } => {
                if !self.filesystem.write {
                    return Err(ErrorCode::Denied.into());
                }
                if self
                    .handles
                    .values()
                    .any(|handle| matches!(handle, Handle::Upload(_)))
                {
                    return Err(ErrorCode::Busy.into());
                }
                let size = rpc::decimal(&size).ok_or(ErrorCode::Invalid)?;
                let (parent, target) = self.filesystem.parent(&path)?;
                let expected = match expected {
                    Some(handle) => {
                        let Some(Handle::Read {
                            path: original,
                            file,
                            stamp: captured,
                        }) = self.handles.get(&handle)
                        else {
                            return Err(ErrorCode::Invalid.into());
                        };
                        if original != &path {
                            return Err(ErrorCode::Invalid.into());
                        }
                        let current = open_file(&parent, &target)?;
                        if !same(file, &current)?
                            || stamp(file)? != *captured
                            || stamp(&current)? != *captured
                        {
                            return Err(ErrorCode::Conflict.into());
                        }
                        Some(Expected {
                            file: file.try_clone()?,
                            stamp: captured.clone(),
                        })
                    }
                    None => {
                        match parent.symlink_metadata(&target) {
                            Ok(_) => return Err(ErrorCode::Exists.into()),
                            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                            Err(error) => return Err(error.into()),
                        }
                        None
                    }
                };
                let temporary = format!("{RESERVED}{}", uuid::Uuid::new_v4().simple());
                let file = private_file(&parent, &temporary)?;
                let handle = self.insert(Handle::Upload(Upload {
                    parent,
                    path,
                    target,
                    temporary,
                    file,
                    size,
                    written: 0,
                    expected,
                    published: false,
                }))?;
                Reply::Upload { handle }
            }
            Operation::Write {
                handle,
                offset,
                length,
            } => {
                let offset = rpc::decimal(&offset).ok_or(ErrorCode::Invalid)?;
                let Some(Handle::Upload(upload)) = self.handles.get_mut(&handle) else {
                    return Err(ErrorCode::Invalid.into());
                };
                if data.len() != length as usize
                    || data.is_empty()
                    || data.len() > rpc::CHUNK_LIMIT
                    || offset != upload.written
                    || data.len() as u64 > upload.size - upload.written
                {
                    return Err(ErrorCode::Invalid.into());
                }
                // A failed/partial write invalidates this upload; do not retry an uncertain offset.
                if let Err(error) = upload.file.write_all(&data) {
                    self.handles.remove(&handle);
                    return Err(error.into());
                }
                upload.written += data.len() as u64;
                Reply::Written {
                    offset: upload.written.to_string(),
                }
            }
            Operation::FinishUpload { handle } => {
                if !matches!(self.handles.get(&handle), Some(Handle::Upload(_))) {
                    return Err(ErrorCode::Invalid.into());
                }
                let Some(Handle::Upload(mut upload)) = self.handles.remove(&handle) else {
                    return Err(ErrorCode::Invalid.into());
                };
                if upload.written != upload.size {
                    return Err(ErrorCode::Invalid.into());
                }
                upload.file.sync_all()?;
                // Final publication admission is distinct from the request job.
                // The callback releases protected policy before native commit.
                admission(AdmissionStage::Publication)?;
                finish(&mut upload, cancel)?;
                Reply::Finished {
                    size: upload.size.to_string(),
                }
            }
        };
        Ok((reply, None))
    }
}

fn finish(upload: &mut Upload, cancel: &CancellationToken) -> Outcome<()> {
    if cancel.is_cancelled() {
        return Err(ErrorCode::Denied.into());
    }
    let Some(expected) = upload.expected.take() else {
        publish(
            &upload.parent,
            &upload.temporary,
            &upload.file,
            &upload.target,
        )?;
        upload.published = true;
        return Ok(());
    };
    let current = open_file(&upload.parent, &upload.target)?;
    if !same(&current, &expected.file)? || stamp(&current)? != expected.stamp {
        return Err(ErrorCode::Conflict.into());
    }
    #[cfg(windows)]
    let current = festerm_windows_security::open_file_no_reparse_for_move(
        &upload.parent.try_clone()?.into_std_file(),
        Path::new(&upload.target),
    )?;
    let backup = format!("{RESERVED}backup-{}", uuid::Uuid::new_v4().simple());
    publish(&upload.parent, &upload.target, &current, &backup)?;
    let result = (|| -> Outcome<()> {
        let captured = open_file(&upload.parent, &backup)?;
        // Rename itself changes ctime/change-time; retain identity, size and
        // mtime checks, then establish a post-rename change stamp.
        let captured_stamp = stamp(&captured)?;
        if !same(&captured, &expected.file)?
            || captured_stamp.length != expected.stamp.length
            || captured_stamp.modified != expected.stamp.modified
        {
            return Err(ErrorCode::Conflict.into());
        }
        if cancel.is_cancelled() {
            return Err(ErrorCode::Denied.into());
        }
        publish(
            &upload.parent,
            &upload.temporary,
            &upload.file,
            &upload.target,
        )?;
        upload.published = true;
        if stamp(&captured).as_ref().ok() != Some(&captured_stamp)
            || remove(&upload.parent, &backup, &current).is_err()
        {
            return Err(recovery(upload, &backup));
        }
        Ok(())
    })();
    if let Err(error) = result {
        if upload.published {
            return Err(error);
        }
        if publish(&upload.parent, &backup, &current, &upload.target).is_err() {
            return Err(recovery(upload, &backup));
        }
        return Err(error);
    }
    Ok(())
}

fn recovery(upload: &Upload, name: &str) -> Failure {
    let parent = upload
        .path
        .rsplit_once('/')
        .map_or("", |(parent, _)| parent);
    Failure {
        code: ErrorCode::RecoveryRequired,
        recovery: Some(format!("{parent}/{name}")),
    }
}

struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub async fn serve(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    filesystem: Arc<Filesystem>,
    expected: Target,
    bytes: &[u8],
) -> io::Result<()> {
    let open: rpc::Open = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if open.version != rpc::VERSION || open.target != expected {
        return Err(invalid());
    }
    serve_authorized(
        stream,
        filesystem,
        Arc::new(|_| Ok(())),
        CancellationToken::new(),
    )
    .await
}

pub(crate) async fn serve_authorized(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    filesystem: Arc<Filesystem>,
    admission: Admission,
    cancel: CancellationToken,
) -> io::Result<()> {
    serve_authorized_mode(stream, filesystem, admission, cancel, false).await
}
#[cfg(any(feature = "host-routing", test))]
pub(crate) async fn serve_authorized_reviewed(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    filesystem: Arc<Filesystem>,
    admission: Admission,
    cancel: CancellationToken,
) -> io::Result<()> {
    serve_authorized_mode(stream, filesystem, admission, cancel, true).await
}
async fn serve_authorized_mode(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    filesystem: Arc<Filesystem>,
    admission: Admission,
    cancel: CancellationToken,
    reviewed_operations: bool,
) -> io::Result<()> {
    // A file close retires only this stream. Global shutdown propagates down
    // through the child; stream retirement must never cancel the HOST parent.
    let cancel = cancel.child_token();
    let _guard = CancelOnDrop(cancel.clone());
    let permit = Arc::new(
        filesystem
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| invalid())?,
    );
    let disclose = admission.clone();
    let charge = permit.clone();
    tokio::time::timeout(
        DEADLINE,
        tokio::task::spawn_blocking(move || {
            let _charge = charge;
            disclose(AdmissionStage::Disclosure)
        }),
    )
    .await
    .map_err(|_| invalid())?
    .map_err(|_| invalid())??;
    let ready = rpc::Response {
        id: 0,
        result: Some(if reviewed_operations {
            Reply::ReadyV3 {
                capabilities: filesystem.operation_capabilities(),
            }
        } else {
            Reply::Ready {
                capabilities: filesystem.capabilities(),
            }
        }),
        error: None,
        recovery_path: None,
    };
    write_frame(&mut stream, rpc::RESPONSE, &serde_json::to_vec(&ready)?).await?;
    let state = Arc::new(Mutex::new(Session {
        reviewed_operations,
        #[cfg(test)]
        admission: admission.clone(),
        filesystem,
        handles: HashMap::new(),
        next: 0,
        _permit: permit,
    }));
    let mut previous = 0;
    loop {
        let (kind, bytes) = tokio::time::timeout(DEADLINE, read_client_frame(&mut stream))
            .await
            .map_err(|_| invalid())??;
        if kind != rpc::REQUEST {
            return Err(invalid());
        }
        let request: rpc::Request = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if request.id <= previous {
            return Err(invalid());
        }
        previous = request.id;
        let data = if let Operation::Write { length, .. } = &request.operation {
            if *length == 0 || *length as usize > rpc::CHUNK_LIMIT {
                return Err(invalid());
            }
            let (kind, bytes) = tokio::time::timeout(DEADLINE, read_client_frame(&mut stream))
                .await
                .map_err(|_| invalid())??;
            if kind != rpc::DATA
                || bytes.len() != *length as usize + 4
                || bytes[..4] != request.id.to_be_bytes()
            {
                return Err(invalid());
            }
            bytes[4..].to_vec()
        } else {
            Vec::new()
        };
        let session = state.clone();
        let token = cancel.clone();
        let deadline = tokio::time::Instant::now() + DEADLINE;
        let admission = scoped_admission(admission.clone(), deadline, token.clone());
        let job = tokio::task::spawn_blocking(move || {
            admission(AdmissionStage::Request)?;
            let result = session
                .lock()
                .map_err(|_| Failure::from(ErrorCode::Io))
                .and_then(|mut session| {
                    // Queue/lock time cannot renew the admission budget.
                    if token.is_cancelled() {
                        return Err(io::Error::from(io::ErrorKind::PermissionDenied).into());
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Err(io::Error::from(io::ErrorKind::TimedOut).into());
                    }
                    session.execute_admitted(request.operation, data, &token, &admission)
                });
            // Admission of this concrete disclosure precedes releasing its
            // result to async framing. No lease crosses filesystem/network I/O.
            admission(AdmissionStage::Disclosure)?;
            Ok::<_, io::Error>(result)
        });
        let result = tokio::time::timeout_at(deadline, job)
            .await
            .map_err(|_| invalid())?
            .map_err(|_| invalid())??;
        let (response, data) = match result {
            Ok((result, data)) => (
                rpc::Response {
                    id: previous,
                    result: Some(result),
                    error: None,
                    recovery_path: None,
                },
                data,
            ),
            Err(error) => (
                rpc::Response {
                    id: previous,
                    result: None,
                    error: Some(error.code),
                    recovery_path: error.recovery,
                },
                None,
            ),
        };
        write_frame(&mut stream, rpc::RESPONSE, &serde_json::to_vec(&response)?).await?;
        if let Some(data) = data {
            let mut bytes = Vec::with_capacity(data.len() + 4);
            bytes.extend_from_slice(&previous.to_be_bytes());
            bytes.extend(data);
            write_frame(&mut stream, rpc::DATA, &bytes).await?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(write: bool) -> (std::path::PathBuf, Session) {
        let path = crate::test_support::private_directory("festerm-rooted-files");
        let filesystem = Filesystem::open(&rpc::Policy {
            start_in: None,
            delete: false,
            root: path.clone(),
            write,
        })
        .unwrap();
        let permit = filesystem.slots.clone().try_acquire_owned().unwrap();
        (
            path,
            Session {
                reviewed_operations: false,
                admission: Arc::new(|_| Ok(())),
                filesystem,
                handles: HashMap::new(),
                next: 0,
                _permit: Arc::new(permit),
            },
        )
    }

    fn handle(reply: Reply) -> u32 {
        match reply {
            Reply::File { handle, .. } | Reply::Upload { handle } | Reply::Directory { handle } => {
                handle
            }
            _ => panic!("missing handle"),
        }
    }

    fn run(session: &mut Session, operation: Operation) -> Reply {
        session
            .execute(operation, Vec::new(), &CancellationToken::new())
            .unwrap_or_else(|error| panic!("{:?}", error.code))
            .0
    }

    #[test]
    fn start_directory_is_nofollow_navigation_not_an_allowed_root() {
        let (path, mut session) = session(false);
        std::fs::create_dir(path.join("child")).unwrap();
        let mut policy = rpc::Policy {
            root: path.clone(),
            start_in: Some("/child".into()),
            write: false,
            delete: false,
        };
        let fs = Filesystem::open(&policy).unwrap();
        session.filesystem = fs;
        assert!(session
            .execute(
                Operation::StartDirectory {},
                vec![],
                &CancellationToken::new()
            )
            .is_err());
        session.reviewed_operations = true;
        assert!(
            matches!(run(&mut session, Operation::StartDirectory {}), Reply::StartDirectory {path} if path.as_str() == "/child")
        );
        assert!(
            session.filesystem.directory("/").is_ok(),
            "start does not narrow root navigation"
        );
        policy.start_in = Some("/missing".into());
        assert!(Filesystem::open(&policy)
            .unwrap()
            .validate_start_directory()
            .is_err());
        std::fs::write(path.join("file"), b"public").unwrap();
        policy.start_in = Some("/file".into());
        assert!(Filesystem::open(&policy)
            .unwrap()
            .validate_start_directory()
            .is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("child", path.join("link")).unwrap();
            policy.start_in = Some("/link".into());
            assert!(Filesystem::open(&policy)
                .unwrap()
                .validate_start_directory()
                .is_err());
        }
        std::fs::remove_dir(path.join("child")).unwrap();
        assert!(
            session
                .execute(
                    Operation::StartDirectory {},
                    vec![],
                    &CancellationToken::new()
                )
                .is_err(),
            "metadata rechecks start existence"
        );
        drop(session);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn display_root_is_reviewed_authenticated_metadata_not_a_path_grant() {
        let (path, mut session) = session(false);
        assert!(
            matches!(session.execute(Operation::DisplayRoot {}, Vec::new(), &CancellationToken::new()), Err(error) if error.code == ErrorCode::Denied)
        );
        session.reviewed_operations = true;
        let Reply::DisplayRoot { root } = run(&mut session, Operation::DisplayRoot {}) else {
            panic!("missing display metadata")
        };
        assert_eq!(root.as_str(), path.to_str().unwrap());
        assert!(session
            .filesystem
            .operation_capabilities()
            .allows_display_root());
        assert!(session.filesystem.directory("/../outside").is_err());
        drop(session);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn rooted_files_enumerate_stream_and_refuse_escape_and_read_only_writes() {
        let (path, mut session) = session(false);
        std::fs::write(path.join("hello"), b"hello").unwrap();
        for path in [
            "/../escape",
            "/a/../../escape",
            "C:/escape",
            "//escape",
            "/a\\escape",
            "/a:stream",
            "/.festerm-transfer-x",
            "/.FESTERM-TRANSFER-x",
        ] {
            assert!(session
                .execute(
                    Operation::OpenRead { path: path.into() },
                    Vec::new(),
                    &CancellationToken::new()
                )
                .is_err());
        }
        let directory = handle(run(
            &mut session,
            Operation::OpenDirectory { path: "/".into() },
        ));
        let Reply::Entries { entries, .. } = run(
            &mut session,
            Operation::ReadDirectory {
                handle: directory,
                limit: 1,
            },
        ) else {
            panic!()
        };
        assert_eq!(entries[0].name, "hello");
        let read = handle(run(
            &mut session,
            Operation::OpenRead {
                path: "/hello".into(),
            },
        ));
        assert!(session
            .execute(
                Operation::FinishUpload { handle: read },
                Vec::new(),
                &CancellationToken::new()
            )
            .is_err());
        let (_, data) = session
            .execute(
                Operation::Read {
                    handle: read,
                    offset: "1".into(),
                    length: 3,
                },
                Vec::new(),
                &CancellationToken::new(),
            )
            .unwrap_or_else(|_| panic!());
        assert_eq!(data.unwrap(), b"ell");
        assert!(session
            .execute(
                Operation::Mkdir {
                    path: "/new".into()
                },
                Vec::new(),
                &CancellationToken::new()
            )
            .is_err());
        assert!(session
            .execute(
                Operation::BeginUpload {
                    path: "/new".into(),
                    size: "1".into(),
                    expected: None
                },
                Vec::new(),
                &CancellationToken::new()
            )
            .is_err());
        drop(session);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn rooted_file_handles_bound_admission_and_release_capacity() {
        let (path, mut session) = session(true);
        run(
            &mut session,
            Operation::Mkdir {
                path: "/folder".into(),
            },
        );
        assert!(path.join("folder").is_dir());
        std::fs::write(path.join("folder").join("file"), b"bounded").unwrap();
        let mut handles = Vec::new();
        for _ in 0..rpc::HANDLE_LIMIT {
            handles.push(handle(run(
                &mut session,
                Operation::OpenRead {
                    path: "/folder/file".into(),
                },
            )));
        }
        let failure = session
            .execute(
                Operation::OpenRead {
                    path: "/folder/file".into(),
                },
                Vec::new(),
                &CancellationToken::new(),
            )
            .unwrap_err();
        assert_eq!(failure.code, ErrorCode::Busy);
        run(&mut session, Operation::Close { handle: handles[0] });
        let next = handle(run(
            &mut session,
            Operation::OpenRead {
                path: "/folder/file".into(),
            },
        ));
        assert!(next > *handles.last().unwrap());
        drop(session);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn rooted_uploads_create_replace_conflict_and_clean_up_without_truncation() {
        let (path, mut session) = session(true);
        let cancel = CancellationToken::new();
        let upload = handle(run(
            &mut session,
            Operation::BeginUpload {
                path: "/new".into(),
                size: "3".into(),
                expected: None,
            },
        ));
        session
            .execute(
                Operation::Write {
                    handle: upload,
                    offset: "0".into(),
                    length: 3,
                },
                b"new".to_vec(),
                &cancel,
            )
            .unwrap_or_else(|_| panic!());
        run(&mut session, Operation::FinishUpload { handle: upload });
        assert_eq!(std::fs::read(path.join("new")).unwrap(), b"new");
        assert!(session
            .execute(
                Operation::BeginUpload {
                    path: "/new".into(),
                    size: "0".into(),
                    expected: None
                },
                Vec::new(),
                &cancel
            )
            .is_err());
        let expected = handle(run(
            &mut session,
            Operation::OpenRead {
                path: "/new".into(),
            },
        ));
        let upload = handle(run(
            &mut session,
            Operation::BeginUpload {
                path: "/new".into(),
                size: "0".into(),
                expected: Some(expected),
            },
        ));
        run(&mut session, Operation::FinishUpload { handle: upload });
        assert_eq!(std::fs::read(path.join("new")).unwrap(), b"");
        run(&mut session, Operation::Close { handle: expected });
        let expected = handle(run(
            &mut session,
            Operation::OpenRead {
                path: "/new".into(),
            },
        ));
        let upload = handle(run(
            &mut session,
            Operation::BeginUpload {
                path: "/new".into(),
                size: "0".into(),
                expected: Some(expected),
            },
        ));
        std::fs::write(path.join("new"), b"other").unwrap();
        assert!(session
            .execute(
                Operation::FinishUpload { handle: upload },
                Vec::new(),
                &cancel
            )
            .is_err());
        assert_eq!(std::fs::read(path.join("new")).unwrap(), b"other");
        run(
            &mut session,
            Operation::BeginUpload {
                path: "/aborted".into(),
                size: "1".into(),
                expected: None,
            },
        );
        drop(session);
        assert!(!path.join("aborted").exists());
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn rooted_uploads_refuse_concurrent_creation_bad_offsets_and_cancelled_publication() {
        let (path, mut session) = session(true);
        let cancel = CancellationToken::new();
        let upload = handle(run(
            &mut session,
            Operation::BeginUpload {
                path: "/race".into(),
                size: "1".into(),
                expected: None,
            },
        ));
        assert!(session
            .execute(
                Operation::Write {
                    handle: upload,
                    offset: "1".into(),
                    length: 1
                },
                vec![1],
                &cancel
            )
            .is_err());
        session
            .execute(
                Operation::Write {
                    handle: upload,
                    offset: "0".into(),
                    length: 1,
                },
                vec![1],
                &cancel,
            )
            .unwrap();
        std::fs::write(path.join("race"), b"concurrent").unwrap();
        assert!(session
            .execute(
                Operation::FinishUpload { handle: upload },
                Vec::new(),
                &cancel
            )
            .is_err());
        assert_eq!(std::fs::read(path.join("race")).unwrap(), b"concurrent");
        let upload = handle(run(
            &mut session,
            Operation::BeginUpload {
                path: "/cancel".into(),
                size: "0".into(),
                expected: None,
            },
        ));
        cancel.cancel();
        assert!(session
            .execute(
                Operation::FinishUpload { handle: upload },
                Vec::new(),
                &cancel
            )
            .is_err());
        drop(session);
        assert!(!path.join("cancel").exists());
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn rooted_file_stream_negotiates_without_terminal_admission_and_transfers_large_files() {
        use crate::{files::FileService, protocol, transport};
        let path = crate::test_support::private_directory("festerm-files-wire");
        let files = FileService::rooted(&rpc::Policy {
            start_in: None,
            delete: false,
            root: path.clone(),
            write: true,
        })
        .unwrap();
        let identity = festerm_sessiond::NativeSessionIdentity {
            name: "no-daemon-files-test".into(),
            pid: std::process::id(),
            created_at_unix_ms: 123,
            endpoint: "no-daemon-endpoint".into(),
        };
        let target = Target {
            name: identity.name.clone(),
            pid: identity.pid,
            generation: "123".into(),
        };
        let (mut client, server) = tokio::io::duplex(8192);
        let disabled_identity = identity.clone();
        let disabled =
            tokio::spawn(async move { transport::serve(server, disabled_identity).await });
        write_frame(
            &mut client,
            rpc::OPEN,
            &serde_json::to_vec(&rpc::Open {
                version: rpc::VERSION,
                target: target.clone(),
            })
            .unwrap(),
        )
        .await
        .unwrap();
        assert!(crate::wire::read_frame(&mut client).await.is_err());
        assert!(disabled.await.unwrap().is_err());
        for version in [protocol::VERSION, rpc::DISCOVERY_VERSION] {
            let (mut client, server) = tokio::io::duplex(128 * 1024);
            let selected = identity.clone();
            let files = files.clone();
            let task =
                tokio::spawn(
                    async move { transport::serve_with_files(server, selected, files).await },
                );
            write_frame(&mut client, protocol::DISCOVER, &version.to_be_bytes())
                .await
                .unwrap();
            let (kind, bytes) = crate::wire::read_frame(&mut client).await.unwrap();
            assert_eq!(kind, protocol::DESCRIPTION);
            if version == protocol::VERSION {
                let description: protocol::Description = serde_json::from_slice(&bytes).unwrap();
                assert!(!description.files.read);
            } else {
                let description: rpc::Discovery = serde_json::from_slice(&bytes).unwrap();
                assert!(description.filesystem.unwrap().write);
            }
            task.await.unwrap().unwrap();
        }
        let (mut client, server) = tokio::io::duplex(128 * 1024);
        let task =
            tokio::spawn(async move { transport::serve_with_files(server, identity, files).await });
        write_frame(
            &mut client,
            rpc::OPEN,
            &serde_json::to_vec(&rpc::Open {
                version: rpc::VERSION,
                target,
            })
            .unwrap(),
        )
        .await
        .unwrap();
        let (_, bytes) = crate::wire::read_frame(&mut client).await.unwrap();
        let ready: rpc::Response = serde_json::from_slice(&bytes).unwrap();
        assert!(matches!(ready.result, Some(Reply::Ready { .. })));
        let chunk = vec![0xA5; rpc::CHUNK_LIMIT];
        let chunks = 65; // Larger than the legacy whole-file snapshot limit.
        let mut id = 1;
        let upload = handle(
            exchange(
                &mut client,
                id,
                Operation::BeginUpload {
                    path: "/large".into(),
                    size: (chunks * chunk.len()).to_string(),
                    expected: None,
                },
                None,
            )
            .await,
        );
        for index in 0..chunks {
            id += 1;
            exchange(
                &mut client,
                id,
                Operation::Write {
                    handle: upload,
                    offset: (index * chunk.len()).to_string(),
                    length: chunk.len() as u32,
                },
                Some(&chunk),
            )
            .await;
        }
        id += 1;
        exchange(
            &mut client,
            id,
            Operation::FinishUpload { handle: upload },
            None,
        )
        .await;
        id += 1;
        let read = handle(
            exchange(
                &mut client,
                id,
                Operation::OpenRead {
                    path: "/large".into(),
                },
                None,
            )
            .await,
        );
        for index in 0..chunks {
            id += 1;
            let result = exchange(
                &mut client,
                id,
                Operation::Read {
                    handle: read,
                    offset: (index * chunk.len()).to_string(),
                    length: chunk.len() as u32,
                },
                None,
            )
            .await;
            assert!(matches!(result, Reply::Read { eof, .. } if eof == (index + 1 == chunks)));
            let (kind, bytes) = crate::wire::read_frame(&mut client).await.unwrap();
            assert_eq!(kind, rpc::DATA);
            assert_eq!(&bytes[..4], &id.to_be_bytes());
            assert_eq!(&bytes[4..], chunk);
        }
        id += 1;
        let eof = exchange(
            &mut client,
            id,
            Operation::Read {
                handle: read,
                offset: (chunks * chunk.len()).to_string(),
                length: 1,
            },
            None,
        )
        .await;
        assert!(matches!(
            eof,
            Reply::Read {
                length: 0,
                eof: true,
                ..
            }
        ));
        let (kind, bytes) = crate::wire::read_frame(&mut client).await.unwrap();
        assert_eq!(kind, rpc::DATA);
        assert_eq!(bytes, id.to_be_bytes());
        drop(client);
        assert!(task.await.unwrap().is_err());
        assert_eq!(
            std::fs::metadata(path.join("large")).unwrap().len(),
            (chunks * chunk.len()) as u64
        );
        std::fs::remove_dir_all(path).unwrap();
    }

    async fn exchange(
        client: &mut tokio::io::DuplexStream,
        id: u32,
        operation: Operation,
        data: Option<&[u8]>,
    ) -> Reply {
        write_frame(
            client,
            rpc::REQUEST,
            &serde_json::to_vec(&rpc::Request { id, operation }).unwrap(),
        )
        .await
        .unwrap();
        if let Some(data) = data {
            let mut bytes = id.to_be_bytes().to_vec();
            bytes.extend_from_slice(data);
            write_frame(client, rpc::DATA, &bytes).await.unwrap();
        }
        let (kind, bytes) = crate::wire::read_frame(client).await.unwrap();
        assert_eq!(kind, rpc::RESPONSE);
        let response: rpc::Response = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(response.id, id);
        assert!(response.error.is_none(), "{:?}", response.error);
        response.result.unwrap()
    }

    #[test]
    fn rooted_publication_has_a_fresh_admission_before_any_replacement_capture() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for allowed in [false, true] {
            let (path, mut session) = session(true);
            std::fs::write(path.join("target"), b"old").unwrap();
            let expected = handle(run(
                &mut session,
                Operation::OpenRead {
                    path: "/target".into(),
                },
            ));
            let upload = handle(run(
                &mut session,
                Operation::BeginUpload {
                    path: "/target".into(),
                    size: "3".into(),
                    expected: Some(expected),
                },
            ));
            session
                .execute(
                    Operation::Write {
                        handle: upload,
                        offset: "0".into(),
                        length: 3,
                    },
                    b"new".to_vec(),
                    &CancellationToken::new(),
                )
                .unwrap();
            let cutover = Arc::new(AtomicBool::new(false));
            let observed = cutover.clone();
            session.admission = Arc::new(move |stage| {
                assert!(stage == AdmissionStage::Publication);
                // Models cutover immediately after this final admission. The
                // already admitted native sequence may still complete.
                observed.store(true, Ordering::SeqCst);
                if allowed {
                    Ok(())
                } else {
                    Err(io::ErrorKind::PermissionDenied.into())
                }
            });
            let result = session.execute(
                Operation::FinishUpload { handle: upload },
                Vec::new(),
                &CancellationToken::new(),
            );
            assert!(cutover.load(Ordering::SeqCst));
            if allowed {
                assert!(result.is_ok());
            } else {
                assert_eq!(result.unwrap_err().code, ErrorCode::Denied);
            }
            assert_eq!(
                std::fs::read(path.join("target")).unwrap(),
                if allowed { b"new" } else { b"old" }
            );
            drop(session);
            assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1);
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    #[tokio::test]
    async fn rooted_cutover_suppresses_disclosure_and_preserves_already_admitted_publication() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for publication in [false, true] {
            let (path, fixture) = session(true);
            let filesystem = fixture.filesystem.clone();
            drop(fixture);
            std::fs::write(path.join("source"), b"source").unwrap();
            let revoked = Arc::new(AtomicBool::new(false));
            let changed = revoked.clone();
            let admission: Admission = Arc::new(move |stage| {
                if stage == AdmissionStage::Disclosure && changed.load(Ordering::SeqCst) {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                if stage
                    == if publication {
                        AdmissionStage::Publication
                    } else {
                        AdmissionStage::Request
                    }
                {
                    changed.store(true, Ordering::SeqCst);
                }
                Ok(())
            });
            let (mut client, server) = tokio::io::duplex(8192);
            let worker = tokio::spawn(serve_authorized(
                server,
                filesystem.clone(),
                admission,
                CancellationToken::new(),
            ));
            let (kind, _) = crate::wire::read_frame(&mut client).await.unwrap();
            assert_eq!(kind, rpc::RESPONSE); // already validated opening seam
            let (id, operation) = if publication {
                let upload = handle(
                    exchange(
                        &mut client,
                        1,
                        Operation::BeginUpload {
                            path: "/published".into(),
                            size: "1".into(),
                            expected: None,
                        },
                        None,
                    )
                    .await,
                );
                exchange(
                    &mut client,
                    2,
                    Operation::Write {
                        handle: upload,
                        offset: "0".into(),
                        length: 1,
                    },
                    Some(b"x"),
                )
                .await;
                (3, Operation::FinishUpload { handle: upload })
            } else {
                (
                    1,
                    Operation::Stat {
                        path: "/source".into(),
                    },
                )
            };
            write_frame(
                &mut client,
                rpc::REQUEST,
                &serde_json::to_vec(&rpc::Request { id, operation }).unwrap(),
            )
            .await
            .unwrap();
            assert!(
                crate::wire::read_frame(&mut client).await.is_err(),
                "revoked disclosure cannot publish a success reply"
            );
            assert!(worker.await.unwrap().is_err());
            assert!(revoked.load(Ordering::SeqCst));
            if publication {
                assert_eq!(std::fs::read(path.join("published")).unwrap(), b"x");
            } else {
                assert!(!path.join("published").exists());
            }
            assert_eq!(std::fs::read(path.join("source")).unwrap(), b"source");
            assert_eq!(filesystem.slots.available_permits(), 2);
            drop(filesystem);
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    #[tokio::test]
    async fn display_root_disclosure_is_fenced_by_current_files_authority() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (path, fixture) = session(false);
        let filesystem = fixture.filesystem.clone();
        drop(fixture);
        let revoked = Arc::new(AtomicBool::new(false));
        let changed = revoked.clone();
        let admission: Admission = Arc::new(move |stage| {
            if stage == AdmissionStage::Request {
                changed.store(true, Ordering::SeqCst);
            }
            if stage == AdmissionStage::Disclosure && changed.load(Ordering::SeqCst) {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(())
        });
        let (mut client, server) = tokio::io::duplex(8192);
        let worker = tokio::spawn(serve_authorized_reviewed(
            server,
            filesystem,
            admission,
            CancellationToken::new(),
        ));
        let (_, greeting) = crate::wire::read_frame(&mut client).await.unwrap();
        let greeting: rpc::Response = serde_json::from_slice(&greeting).unwrap();
        assert!(
            matches!(greeting.result, Some(Reply::ReadyV3 { capabilities }) if capabilities.allows_display_root())
        );
        write_frame(
            &mut client,
            rpc::REQUEST,
            &serde_json::to_vec(&rpc::Request {
                id: 1,
                operation: Operation::DisplayRoot {},
            })
            .unwrap(),
        )
        .await
        .unwrap();
        assert!(crate::wire::read_frame(&mut client).await.is_err());
        assert!(worker.await.unwrap().is_err());
        assert!(revoked.load(Ordering::SeqCst));
        std::fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn rooted_wire_busy_then_policy_change_refuses_request_and_disclosure() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        for stage in [AdmissionStage::Request, AdmissionStage::Disclosure] {
            let (path, fixture) = session(true);
            let filesystem = fixture.filesystem.clone();
            drop(fixture);
            std::fs::write(path.join("canary"), b"old").unwrap();
            let armed = Arc::new(AtomicBool::new(false));
            let active = armed.clone();
            let calls = Arc::new(AtomicUsize::new(0));
            let observed = calls.clone();
            let admission: Admission = Arc::new(move |current| {
                if current == stage && active.load(Ordering::SeqCst) {
                    return Err(if observed.fetch_add(1, Ordering::SeqCst) == 0 {
                        io::ErrorKind::WouldBlock
                    } else {
                        io::ErrorKind::PermissionDenied
                    }
                    .into());
                }
                Ok(())
            });
            let parent = CancellationToken::new();
            let (mut client, server) = tokio::io::duplex(8192);
            let worker = tokio::spawn(serve_authorized(
                server,
                filesystem.clone(),
                admission,
                parent.clone(),
            ));
            crate::wire::read_frame(&mut client).await.unwrap();
            armed.store(true, Ordering::SeqCst);
            write_frame(
                &mut client,
                rpc::REQUEST,
                &serde_json::to_vec(&rpc::Request {
                    id: 1,
                    operation: Operation::BeginUpload {
                        path: "/new".into(),
                        size: "0".into(),
                        expected: None,
                    },
                })
                .unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(
                crate::wire::read_frame(&mut client)
                    .await
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::UnexpectedEof
            );
            assert_eq!(
                worker.await.unwrap().unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            assert!(!parent.is_cancelled());
            assert_eq!(filesystem.slots.available_permits(), 2);
            assert!(!path.join("new").exists());
            assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1);
            assert_eq!(std::fs::read(path.join("canary")).unwrap(), b"old");
            drop(client);
            drop(filesystem);
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn rooted_scoped_publication_never_reuses_approval_after_busy_cancel_or_expiry() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        // Exercise the actual replacement publication boundary, not a formatter
        // or a synthetic model of the filesystem operation.
        for mode in ["changed", "cancelled", "fatal", "allowed"] {
            let (path, mut session) = session(true);
            std::fs::write(path.join("target"), b"old").unwrap();
            let expected = handle(run(
                &mut session,
                Operation::OpenRead {
                    path: "/target".into(),
                },
            ));
            let upload = handle(run(
                &mut session,
                Operation::BeginUpload {
                    path: "/target".into(),
                    size: "3".into(),
                    expected: Some(expected),
                },
            ));
            session
                .execute(
                    Operation::Write {
                        handle: upload,
                        offset: "0".into(),
                        length: 3,
                    },
                    b"new".to_vec(),
                    &CancellationToken::new(),
                )
                .unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let observed = calls.clone();
            let cancel = CancellationToken::new();
            let cancellation = cancel.clone();
            let base: Admission = Arc::new(move |stage| {
                assert!(stage == AdmissionStage::Publication);
                let call = observed.fetch_add(1, Ordering::SeqCst);
                if mode == "fatal" || (mode == "changed" && call > 0) {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                if mode == "cancelled" {
                    cancellation.cancel();
                }
                if call == 0 {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                Ok(())
            });
            let deadline = tokio::time::Instant::now() + DEADLINE;
            let admission = scoped_admission(base, deadline, cancel.clone());
            let result = session.execute_admitted(
                Operation::FinishUpload { handle: upload },
                Vec::new(),
                &cancel,
                &admission,
            );
            assert_eq!(result.is_ok(), mode == "allowed");
            assert_eq!(
                calls.load(Ordering::SeqCst),
                if mode == "allowed" || mode == "changed" {
                    2
                } else {
                    1
                }
            );
            assert_eq!(
                std::fs::read(path.join("target")).unwrap(),
                if mode == "allowed" { b"new" } else { b"old" }
            );
            drop(session);
            assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1);
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn rooted_publication_busy_uses_original_budget_without_late_read_or_mutation() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (path, mut session) = session(true);
        std::fs::write(path.join("target"), b"old").unwrap();
        let expected = handle(run(
            &mut session,
            Operation::OpenRead {
                path: "/target".into(),
            },
        ));
        let upload = handle(run(
            &mut session,
            Operation::BeginUpload {
                path: "/target".into(),
                size: "3".into(),
                expected: Some(expected),
            },
        ));
        session
            .execute(
                Operation::Write {
                    handle: upload,
                    offset: "0".into(),
                    length: 3,
                },
                b"new".to_vec(),
                &CancellationToken::new(),
            )
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let reads = calls.clone();
        let (started, seen) = tokio::sync::oneshot::channel();
        let started = Mutex::new(Some(started));
        let (release, held) = std::sync::mpsc::channel();
        let held = Mutex::new(held);
        let callback: Admission = Arc::new(move |stage| {
            assert!(stage == AdmissionStage::Publication);
            reads.fetch_add(1, Ordering::SeqCst);
            started.lock().unwrap().take().unwrap().send(()).unwrap();
            held.lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap();
            Err(io::ErrorKind::WouldBlock.into())
        });
        let deadline = tokio::time::Instant::now() + DEADLINE;
        let cancel = CancellationToken::new();
        let admission = scoped_admission(callback, deadline, cancel.clone());
        let job = tokio::task::spawn_blocking(move || {
            let result = session.execute_admitted(
                Operation::FinishUpload { handle: upload },
                Vec::new(),
                &cancel,
                &admission,
            );
            (session, result)
        });
        seen.await.unwrap();
        // Expire the original clock while the actual publication read is held;
        // no real 30-second sleep and no second budget or callback invocation.
        tokio::time::advance(DEADLINE).await;
        release.send(()).unwrap();
        let (session, result) = job.await.unwrap();
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read(path.join("target")).unwrap(), b"old");
        drop(session);
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn rooted_scoped_admission_rejects_queued_expired_and_post_read_cancelled_work() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for stage in [
            AdmissionStage::Request,
            AdmissionStage::Disclosure,
            AdmissionStage::Publication,
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let read = calls.clone();
            let callback: Admission = Arc::new(move |_| {
                read.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
            let expired = scoped_admission(
                callback,
                tokio::time::Instant::now(),
                CancellationToken::new(),
            );
            assert_eq!(expired(stage).unwrap_err().kind(), io::ErrorKind::TimedOut);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            let cancel = CancellationToken::new();
            let during_read = cancel.clone();
            let callback: Admission = Arc::new(move |_| {
                during_read.cancel();
                Ok(())
            });
            let cancelled =
                scoped_admission(callback, tokio::time::Instant::now() + DEADLINE, cancel);
            assert_eq!(
                cancelled(stage).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
        }
    }

    #[tokio::test]
    async fn rooted_transfer_survives_short_competing_policy_read_without_replay() {
        use fs2::FileExt;
        use std::sync::atomic::{AtomicBool, Ordering};

        struct ReadLease(File);
        impl Drop for ReadLease {
            fn drop(&mut self) {
                let _ = FileExt::unlock(&self.0);
            }
        }
        fn policy_read(
            directory: &File,
            busy: Option<&Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
        ) -> io::Result<ReadLease> {
            // Distinct opens, not duplicated descriptors: the real exclusive
            // lifecycle-read primitive must actually contend across readers.
            let file = crate::setup_storage::open_private_file(
                directory,
                Path::new("host-lifecycle.lock"),
                false,
            )?;
            if let Err(error) = file.try_lock_exclusive() {
                if fs2::lock_contended_error()
                    .raw_os_error()
                    .is_some_and(|code| error.raw_os_error() == Some(code))
                {
                    if let Some(sender) = busy.and_then(|slot| slot.lock().unwrap().take()) {
                        let _ = sender.send(());
                    }
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "fixture.policy_read_busy",
                    ));
                }
                return Err(error);
            }
            let lease = ReadLease(file);
            if crate::setup_storage::read_private(directory, Path::new("policy"), 64)?.as_slice()
                != b"same scope/root/grant"
            {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(lease)
        }

        let (path, fixture) = session(true);
        let filesystem = fixture.filesystem.clone();
        drop(fixture);
        std::fs::write(path.join("canary"), b"original").unwrap();
        let policy_path = crate::test_support::private_directory("festerm-policy-read-contention");
        let directory =
            Arc::new(crate::setup_storage::private_directory(&policy_path, false).unwrap());
        drop(
            crate::setup_storage::open_private_file(
                &directory,
                Path::new("host-lifecycle.lock"),
                true,
            )
            .unwrap(),
        );
        crate::setup_storage::write_new_private(
            &directory,
            Path::new("policy"),
            b"same scope/root/grant",
        )
        .unwrap();
        let (busy_sender, busy_seen) = tokio::sync::oneshot::channel();
        let busy_sender = Mutex::new(Some(busy_sender));
        let transfer_directory = directory.clone();
        let transfer_admission: Admission = Arc::new(move |_| {
            let _lease = policy_read(&transfer_directory, Some(&busy_sender))?;
            Ok(())
        });
        let hold = Arc::new(AtomicBool::new(false));
        let hold_read = hold.clone();
        let (held_sender, held_seen) = tokio::sync::oneshot::channel();
        let held_sender = Mutex::new(Some(held_sender));
        let (release, released) = std::sync::mpsc::channel();
        let released = Mutex::new(released);
        let browse_directory = directory.clone();
        let browse_admission: Admission = Arc::new(move |stage| {
            let _lease = policy_read(&browse_directory, None)?;
            if stage == AdmissionStage::Request && hold_read.swap(false, Ordering::AcqRel) {
                held_sender
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap()
                    .send(())
                    .unwrap();
                // Emergency fixture bound only; normal release is synchronized
                // with the competing transfer's actual OS-lock WouldBlock.
                released
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(3))
                    .unwrap();
            }
            Ok(())
        });
        let parent = CancellationToken::new();
        let (mut transfer, server) = tokio::io::duplex(8192);
        let transfer_worker = tokio::spawn(serve_authorized(
            server,
            filesystem.clone(),
            transfer_admission,
            parent.clone(),
        ));
        crate::wire::read_frame(&mut transfer).await.unwrap();
        let (mut browse, server) = tokio::io::duplex(8192);
        let browse_worker = tokio::spawn(serve_authorized(
            server,
            filesystem.clone(),
            browse_admission,
            parent.clone(),
        ));
        crate::wire::read_frame(&mut browse).await.unwrap();
        assert_eq!(filesystem.slots.available_permits(), 0);
        let upload = handle(
            exchange(
                &mut transfer,
                1,
                Operation::BeginUpload {
                    path: "/owned".into(),
                    size: "2".into(),
                    expected: None,
                },
                None,
            )
            .await,
        );
        assert!(matches!(exchange(&mut transfer, 2, Operation::Write {
            handle: upload, offset: "0".into(), length: 1,
        }, Some(b"a")).await, Reply::Written { offset } if offset == "1"));
        assert!(!path.join("owned").exists());

        hold.store(true, Ordering::Release);
        let browse_ping = tokio::spawn(async move {
            assert!(matches!(
                exchange(&mut browse, 1, Operation::Ping {}, None).await,
                Reply::Pong
            ));
            browse
        });
        held_seen.await.unwrap();
        let transfer_request = tokio::spawn(async move {
            let request = rpc::Request {
                id: 3,
                operation: Operation::Write {
                    handle: upload,
                    offset: "1".into(),
                    length: 1,
                },
            };
            write_frame(
                &mut transfer,
                rpc::REQUEST,
                &serde_json::to_vec(&request).unwrap(),
            )
            .await
            .unwrap();
            write_frame(&mut transfer, rpc::DATA, &[0, 0, 0, 3, b'b'])
                .await
                .unwrap();
            let result = crate::wire::read_frame(&mut transfer).await;
            (transfer, result)
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), busy_seen)
            .await
            .unwrap()
            .unwrap();
        release.send(()).unwrap();
        let mut browse = browse_ping.await.unwrap();
        let (mut transfer, result) = transfer_request.await.unwrap();
        let result_kind = result.as_ref().err().map(io::Error::kind);
        if let Ok((kind, bytes)) = &result {
            assert_eq!(*kind, rpc::RESPONSE);
            let response: rpc::Response = serde_json::from_slice(bytes).unwrap();
            assert_eq!(response.id, 3);
            assert!(response.error.is_none());
            assert!(matches!(response.result, Some(Reply::Written { offset }) if offset == "2"));
            assert!(
                matches!(exchange(&mut transfer, 4, Operation::FinishUpload { handle: upload }, None).await, Reply::Finished { size } if size == "2")
            );
            assert_eq!(std::fs::read(path.join("owned")).unwrap(), b"ab");
        }
        // A refused/closed transfer must not cancel its sibling or parent.
        assert!(!parent.is_cancelled());
        assert!(matches!(
            exchange(&mut browse, 2, Operation::Ping {}, None).await,
            Reply::Pong
        ));
        drop(transfer);
        drop(browse);
        let transfer_exit = transfer_worker.await.unwrap();
        let _ = browse_worker.await.unwrap();
        assert!(!parent.is_cancelled());
        assert_eq!(filesystem.slots.available_permits(), 2);
        assert_eq!(std::fs::read(path.join("canary")).unwrap(), b"original");
        assert!(!std::fs::read_dir(&path).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(RESERVED)));
        drop(filesystem);
        drop(directory);
        std::fs::remove_dir_all(path).unwrap();
        std::fs::remove_dir_all(policy_path).unwrap();
        assert!(result.is_ok(), "one transfer RPC must survive released read contention; client={result_kind:?}, server={:?}", transfer_exit.err().map(|error| error.kind()));
    }

    #[tokio::test]
    async fn rooted_abandoned_native_admission_retains_its_shared_stream_permit() {
        let (path, fixture) = session(true);
        let filesystem = fixture.filesystem.clone();
        drop(fixture);
        let (started, waiting) = tokio::sync::oneshot::channel();
        let started = Mutex::new(Some(started));
        let (release, released) = std::sync::mpsc::channel();
        let released = Mutex::new(released);
        let admission: Admission = Arc::new(move |stage| {
            if stage == AdmissionStage::Request {
                started.lock().unwrap().take().unwrap().send(()).unwrap();
                released.lock().unwrap().recv().unwrap();
            }
            Ok(())
        });
        let (mut client, server) = tokio::io::duplex(8192);
        let worker = tokio::spawn(serve_authorized(
            server,
            filesystem.clone(),
            admission,
            CancellationToken::new(),
        ));
        crate::wire::read_frame(&mut client).await.unwrap();
        write_frame(
            &mut client,
            rpc::REQUEST,
            &serde_json::to_vec(&rpc::Request {
                id: 1,
                operation: Operation::Ping {},
            })
            .unwrap(),
        )
        .await
        .unwrap();
        waiting.await.unwrap();
        worker.abort();
        let _ = worker.await;
        assert_eq!(filesystem.slots.available_permits(), 1);
        let other = filesystem.slots.clone().try_acquire_owned().unwrap();
        assert_eq!(filesystem.slots.available_permits(), 0);
        release.send(()).unwrap();
        let finished = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            filesystem.slots.clone().acquire_owned(),
        )
        .await
        .unwrap()
        .unwrap();
        drop(finished);
        drop(other);
        assert_eq!(filesystem.slots.available_permits(), 2);
        drop(filesystem);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn rooted_files_refuse_windows_junction_escape() {
        let (path, mut session) = session(true);
        let outside = crate::test_support::private_directory("festerm-files-outside");
        std::fs::write(outside.join("outside"), b"outside").unwrap();
        let junction = path.join("escape");
        let result = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        for operation in [
            Operation::OpenDirectory {
                path: "/escape".into(),
            },
            Operation::OpenRead {
                path: "/escape/outside".into(),
            },
            Operation::BeginUpload {
                path: "/escape/new".into(),
                size: "0".into(),
                expected: None,
            },
        ] {
            assert!(session
                .execute(operation, Vec::new(), &CancellationToken::new())
                .is_err());
        }
        drop(session);
        std::fs::remove_dir(junction).unwrap();
        std::fs::remove_dir_all(path).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rooted_files_refuse_symlink_escape() {
        use std::os::unix::fs::symlink;
        let (path, mut session) = session(true);
        symlink("/", path.join("escape")).unwrap();
        for operation in [
            Operation::OpenDirectory {
                path: "/escape".into(),
            },
            Operation::OpenRead {
                path: "/escape/etc/passwd".into(),
            },
            Operation::BeginUpload {
                path: "/escape/new".into(),
                size: "0".into(),
                expected: None,
            },
        ] {
            assert!(session
                .execute(operation, Vec::new(), &CancellationToken::new())
                .is_err());
        }
        drop(session);
        std::fs::remove_dir_all(path).unwrap();
    }
}
