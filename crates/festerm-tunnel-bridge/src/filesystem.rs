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

const RESERVED: &str = ".festerm-transfer-";

pub struct Filesystem {
    root: Dir,
    write: bool,
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
        Ok(Arc::new(Self {
            root: directory,
            write: policy.write,
            slots: Arc::new(Semaphore::new(2)),
        }))
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
    if name.is_empty()
        || name.len() > 255
        || matches!(name, "." | "..")
        || name.to_ascii_lowercase().starts_with(RESERVED)
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        || name.ends_with(['.', ' '])
    {
        return false;
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) && !(stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.as_bytes()[3].is_ascii_digit())
}

pub(crate) fn open_file(parent: &Dir, name: &str) -> io::Result<File> {
    let parent = parent.try_clone()?.into_std_file();
    #[cfg(unix)]
    return festerm_unix_security::open_file_nofollow(&parent, Path::new(name));
    #[cfg(windows)]
    return festerm_windows_security::open_file_no_reparse_for_capture(&parent, Path::new(name));
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
    let metadata = file.metadata()?;
    if !metadata.is_file() {
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
        length: metadata.len(),
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

struct Session {
    filesystem: Arc<Filesystem>,
    handles: HashMap<u32, Handle>,
    next: u32,
    _permit: OwnedSemaphorePermit,
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

    fn execute(
        &mut self,
        operation: Operation,
        data: Vec<u8>,
        cancel: &CancellationToken,
    ) -> Outcome<(Reply, Option<Vec<u8>>)> {
        if cancel.is_cancelled() {
            return Err(ErrorCode::Denied.into());
        }
        let reply = match operation {
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
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    filesystem: Arc<Filesystem>,
    expected: Target,
    bytes: &[u8],
) -> io::Result<()> {
    let open: rpc::Open = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if open.version != rpc::VERSION || open.target != expected {
        return Err(invalid());
    }
    let permit = filesystem
        .slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| invalid())?;
    let ready = rpc::Response {
        id: 0,
        result: Some(Reply::Ready {
            capabilities: filesystem.capabilities(),
        }),
        error: None,
        recovery_path: None,
    };
    write_frame(&mut stream, rpc::RESPONSE, &serde_json::to_vec(&ready)?).await?;
    let state = Arc::new(Mutex::new(Session {
        filesystem,
        handles: HashMap::new(),
        next: 0,
        _permit: permit,
    }));
    let cancel = CancellationToken::new();
    let _guard = CancelOnDrop(cancel.clone());
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
        let job = tokio::task::spawn_blocking(move || {
            session
                .lock()
                .map_err(|_| Failure::from(ErrorCode::Io))?
                .execute(request.operation, data, &token)
        });
        let result = tokio::time::timeout(DEADLINE, job)
            .await
            .map_err(|_| invalid())?
            .map_err(|_| invalid())?;
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
            root: path.clone(),
            write,
        })
        .unwrap();
        let permit = filesystem.slots.clone().try_acquire_owned().unwrap();
        (
            path,
            Session {
                filesystem,
                handles: HashMap::new(),
                next: 0,
                _permit: permit,
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
