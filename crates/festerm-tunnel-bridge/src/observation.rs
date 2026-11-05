//! Bounded native observation over retained, trusted rooted capabilities.
use crate::{
    file_protocol::{ErrorCode, Policy as FilePolicy},
    filesystem::{open_observation_file as open_file, valid_name, Filesystem},
    observation_protocol::{self as rpc, Operation, Result as Reply},
    protocol::{invalid, Target},
    wire::{read_client_frame, write_frame, DEADLINE},
};
use cap_std::fs::{Dir, ReadDir};
use festerm_sessiond::NativeSessionIdentity;
use notify::{event::ModifyKind, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::{HashMap, VecDeque},
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{OwnedSemaphorePermit, Semaphore},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Root {
    id: String,
    label: String,
    path: PathBuf,
    filesystem: Arc<Filesystem>,
}

pub struct Observation {
    roots: Vec<Root>,
    slots: Arc<Semaphore>,
    jobs: Arc<Semaphore>,
}

fn nonce() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

impl Observation {
    pub fn open(policy: &rpc::Policy) -> io::Result<Option<Arc<Self>>> {
        if policy.roots.len() > rpc::ROOT_LIMIT {
            return Err(invalid());
        }
        if policy.roots.is_empty() {
            return Ok(None);
        }
        let mut roots = Vec::new();
        for path in &policy.roots {
            if path.as_os_str().len() > 4096 {
                return Err(invalid());
            }
            if roots.iter().any(|root: &Root| &root.path == path) {
                return Err(invalid());
            }
            let filesystem = Filesystem::open(&FilePolicy {
                root: path.clone(),
                write: false,
            })?;
            let label = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("Approved root");
            if label.len() > 255 || label.chars().any(char::is_control) {
                return Err(invalid());
            }
            roots.push(Root {
                id: nonce(),
                label: label.into(),
                path: path.clone(),
                filesystem,
            });
        }
        Ok(Some(Arc::new(Self {
            roots,
            slots: Arc::new(Semaphore::new(rpc::STREAM_LIMIT)),
            jobs: Arc::new(Semaphore::new(rpc::STREAM_LIMIT)),
        })))
    }

    pub fn capabilities(&self, correlation: bool) -> rpc::Capabilities {
        rpc::Capabilities {
            version: rpc::VERSION,
            roots: self
                .roots
                .iter()
                .map(|r| rpc::Root {
                    id: r.id.clone(),
                    label: r.label.clone(),
                })
                .collect(),
            chunk_limit: rpc::CHUNK_LIMIT,
            page_limit: rpc::PAGE_LIMIT,
            watch: cfg!(any(windows, target_os = "linux")),
            correlation,
        }
    }

    pub(crate) async fn live_capabilities(
        &self,
        selected: NativeSessionIdentity,
    ) -> io::Result<rpc::Capabilities> {
        let permit = self
            .jobs
            .clone()
            .try_acquire_owned()
            .map_err(|_| invalid())?;
        let correlation = tokio::time::timeout(
            DEADLINE,
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let root = festerm_sessiond::runtime_root().map_err(|_| invalid())?;
                festerm_sessiond::session_correlation_in(&root, &selected).map_err(|_| invalid())
            }),
        )
        .await
        .map_err(|_| invalid())?
        .map_err(|_| invalid())??;
        Ok(self.capabilities(correlation.is_some()))
    }

    fn root(&self, id: &str) -> io::Result<&Root> {
        self.roots.iter().find(|r| r.id == id).ok_or_else(invalid)
    }
}

#[derive(Clone)]
struct Generation {
    root: PathBuf,
    selected: NativeSessionIdentity,
}
impl Generation {
    fn check(&self, cancel: &CancellationToken) -> io::Result<Option<rpc::Correlation>> {
        if cancel.is_cancelled() {
            return Err(invalid());
        }
        let value = festerm_sessiond::session_correlation_in(&self.root, &self.selected)
            .map_err(|_| invalid())?;
        if cancel.is_cancelled() {
            return Err(invalid());
        }
        Ok(value.map(|value| rpc::Correlation { value }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    identity: String,
    size: u64,
    modified: Option<std::time::SystemTime>,
    changed: i128,
    kind: rpc::Kind,
}
fn stamp(file: &File) -> io::Result<Stamp> {
    let metadata = file.metadata()?;
    let kind = if metadata.is_file() {
        rpc::Kind::File
    } else if metadata.is_dir() {
        rpc::Kind::Directory
    } else {
        return Err(invalid());
    };
    #[cfg(unix)]
    let (identity, changed) = {
        use std::os::unix::fs::MetadataExt;
        (
            format!("{}:{}", metadata.dev(), metadata.ino()),
            i128::from(metadata.ctime()) * 1_000_000_000 + i128::from(metadata.ctime_nsec()),
        )
    };
    #[cfg(windows)]
    let (identity, changed) = {
        let (volume, index) = festerm_windows_security::file_identity(file)?;
        (
            format!("{volume}:{index}"),
            i128::from(festerm_windows_security::file_change_time(file)?),
        )
    };
    Ok(Stamp {
        identity,
        size: metadata.len(),
        modified: metadata.modified().ok(),
        changed,
        kind,
    })
}
fn describe(stamp: &Stamp, revision: String) -> rpc::Metadata {
    rpc::Metadata {
        kind: stamp.kind.clone(),
        identity: stamp.identity.clone(),
        size: stamp.size.to_string(),
        modified_ms: stamp
            .modified
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .and_then(|t| u64::try_from(t.as_millis()).ok())
            .map(|t| t.to_string()),
        revision,
    }
}
fn object(filesystem: &Filesystem, path: &str) -> io::Result<File> {
    if !rpc::valid_path(path) {
        return Err(invalid());
    }
    match filesystem.directory(path) {
        Ok(directory) => Ok(directory.into_std_file()),
        Err(_) => {
            let (parent, name) = filesystem.parent(path)?;
            open_file(&parent, &name)
        }
    }
}

struct Pinned {
    _file: File,
    stamp: Stamp,
    metadata: rpc::Metadata,
}
struct Cursor {
    root: String,
    path: String,
    directory: Dir,
    iterator: ReadDir,
}
struct State {
    service: Arc<Observation>,
    files: HashMap<(String, String), Pinned>,
    order: VecDeque<(String, String)>,
    cursors: HashMap<String, Cursor>,
    _permit: OwnedSemaphorePermit,
}

impl State {
    fn metadata(&mut self, root: &str, path: &str, file: &File) -> io::Result<rpc::Metadata> {
        let current = stamp(file)?;
        let key = (root.to_owned(), path.to_owned());
        if let Some(pinned) = self.files.get(&key) {
            if current == pinned.stamp {
                return Ok(pinned.metadata.clone());
            }
        }
        let metadata = describe(&current, nonce());
        if !self.files.contains_key(&key) {
            if self.files.len() >= rpc::FILE_LIMIT {
                if let Some(old) = self.order.pop_front() {
                    self.files.remove(&old);
                }
            }
            self.order.push_back(key.clone());
        }
        self.files.insert(
            key,
            Pinned {
                _file: file.try_clone()?,
                stamp: current,
                metadata: metadata.clone(),
            },
        );
        Ok(metadata)
    }

    fn execute(
        &mut self,
        root_id: &str,
        operation: Operation,
        cancel: &CancellationToken,
    ) -> io::Result<(Reply, Option<Vec<u8>>)> {
        if cancel.is_cancelled() {
            return Err(invalid());
        }
        let filesystem = self.service.root(root_id)?.filesystem.clone();
        let result = match operation {
            Operation::Ping {} => Reply::Pong,
            Operation::Stat { path } => {
                let file = object(&filesystem, &path)?;
                Reply::Stat {
                    metadata: self.metadata(root_id, &path, &file)?,
                }
            }
            Operation::OpenDirectory { path } => {
                if !rpc::valid_path(&path) || self.cursors.len() >= rpc::CURSOR_LIMIT {
                    return Err(invalid());
                }
                let directory = filesystem.directory(&path)?;
                let iterator = directory.entries()?;
                let cursor = nonce();
                self.cursors.insert(
                    cursor.clone(),
                    Cursor {
                        root: root_id.into(),
                        path,
                        directory,
                        iterator,
                    },
                );
                Reply::Directory { cursor }
            }
            Operation::ReadDirectory { cursor, limit } => {
                if limit == 0 || limit > rpc::PAGE_LIMIT || cursor.len() != 32 {
                    return Err(invalid());
                }
                let mut cursor = self
                    .cursors
                    .remove(&cursor)
                    .map(|c| (cursor, c))
                    .ok_or_else(invalid)?;
                if cursor.1.root != root_id {
                    self.cursors.insert(cursor.0, cursor.1);
                    return Err(invalid());
                }
                let mut page = rpc::Page {
                    entries: Vec::new(),
                    done: false,
                    skipped: 0,
                };
                // Bound work even when every entry is unreadable.
                for _ in 0..limit {
                    let Some(entry) = cursor.1.iterator.next() else {
                        page.done = true;
                        break;
                    };
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(_) => {
                            page.skipped += 1;
                            continue;
                        }
                    };
                    let name = entry.file_name();
                    let Some(name) = name.to_str().filter(|name| valid_name(name)) else {
                        page.skipped += 1;
                        continue;
                    };
                    let path = format!("{}/{}", cursor.1.path.trim_end_matches('/'), name);
                    if path.len() > 4096 {
                        page.skipped += 1;
                        continue;
                    }
                    // Open relative to the retained iterator directory, not a name-based stat.
                    let file = cursor
                        .1
                        .directory
                        .open_dir_nofollow_observation(name)
                        .or_else(|_| open_file(&cursor.1.directory, name));
                    match file.and_then(|file| self.metadata(root_id, &path, &file)) {
                        Ok(metadata) => page.entries.push(rpc::Entry {
                            name: name.into(),
                            metadata,
                        }),
                        Err(_) => page.skipped += 1,
                    }
                }
                self.cursors.insert(cursor.0, cursor.1);
                Reply::Page { page }
            }
            Operation::CloseDirectory { cursor } => {
                if self.cursors.get(&cursor).is_none_or(|c| c.root != root_id) {
                    return Err(invalid());
                }
                self.cursors.remove(&cursor);
                Reply::Closed
            }
            Operation::Read {
                path,
                offset,
                length,
                expected,
            } => {
                let offset = rpc::decimal(&offset)?;
                expected.validate()?;
                if length as usize > rpc::CHUNK_LIMIT {
                    return Err(invalid());
                }
                let mut file = object(&filesystem, &path)?;
                let metadata = self.metadata(root_id, &path, &file)?;
                if metadata.kind != rpc::Kind::File {
                    return Err(invalid());
                }
                if metadata != expected {
                    return Ok((Reply::Changed { metadata }, None));
                }
                let before = stamp(&file)?;
                file.seek(SeekFrom::Start(offset))?;
                let mut data = vec![0; length as usize];
                let mut count = 0;
                while count < data.len() {
                    if cancel.is_cancelled() {
                        return Err(invalid());
                    }
                    let n = file.read(&mut data[count..])?;
                    if n == 0 {
                        break;
                    }
                    count += n;
                }
                data.truncate(count);
                let reopened = object(&filesystem, &path)?;
                let after = stamp(&file)?;
                let current = self.metadata(root_id, &path, &reopened)?;
                if before != after || current != metadata {
                    // Even if the path changed away and back, retire its revision.
                    self.files.remove(&(root_id.into(), path.clone()));
                    self.order
                        .retain(|key| key != &(root_id.into(), path.clone()));
                    let metadata = self.metadata(root_id, &path, &reopened)?;
                    return Ok((Reply::Changed { metadata }, None));
                }
                if cancel.is_cancelled() {
                    return Err(invalid());
                }
                return Ok((
                    Reply::Data {
                        metadata,
                        length: count as u32,
                    },
                    Some(data),
                ));
            }
            Operation::Watch {} | Operation::Correlation {} => return Err(invalid()),
        };
        if cancel.is_cancelled() {
            return Err(invalid());
        }
        Ok((result, None))
    }
}

// Reuse the same no-follow/trusted-directory policy for native list entries.
trait ObservationDirectory {
    fn open_dir_nofollow_observation(&self, name: &str) -> io::Result<File>;
}
impl ObservationDirectory for Dir {
    fn open_dir_nofollow_observation(&self, name: &str) -> io::Result<File> {
        use cap_fs_ext::DirExt;
        let directory = self.open_dir_nofollow(name)?;
        let file = directory.into_std_file();
        #[cfg(unix)]
        festerm_unix_security::trusted_private_storage_ancestor(&file)?;
        #[cfg(windows)]
        festerm_windows_security::trusted_private_storage_ancestor(&file)?;
        Ok(file)
    }
}

struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn error_code(error: &io::Error) -> ErrorCode {
    match error.kind() {
        io::ErrorKind::NotFound => ErrorCode::NotFound,
        io::ErrorKind::PermissionDenied => ErrorCode::Denied,
        io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput => ErrorCode::Invalid,
        _ => ErrorCode::Io,
    }
}

pub async fn serve(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    service: Arc<Observation>,
    selected: NativeSessionIdentity,
    expected: Target,
    bytes: &[u8],
) -> io::Result<()> {
    let generation = Generation {
        root: festerm_sessiond::runtime_root().map_err(|_| invalid())?,
        selected,
    };
    serve_generation(stream, service, generation, expected, bytes).await
}

async fn serve_generation(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    service: Arc<Observation>,
    generation: Generation,
    expected: Target,
    bytes: &[u8],
) -> io::Result<()> {
    let open: rpc::Open = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if open.version != rpc::VERSION || open.target != expected {
        return Err(invalid());
    }
    let permit = service
        .slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| invalid())?;
    let cancel = CancellationToken::new();
    let _guard = CancelOnDrop(cancel.clone());
    let checked = generation.clone();
    let token = cancel.clone();
    let job_permit = service
        .jobs
        .clone()
        .try_acquire_owned()
        .map_err(|_| invalid())?;
    let correlation = tokio::task::spawn_blocking(move || {
        let _permit = job_permit;
        checked.check(&token)
    })
    .await
    .map_err(|_| invalid())??;
    let ready = rpc::Response {
        id: 0,
        result: Some(Reply::Ready {
            capabilities: service.capabilities(correlation.is_some()),
        }),
        error: None,
    };
    write_frame(&mut stream, rpc::RESPONSE, &serde_json::to_vec(&ready)?).await?;
    let state = Arc::new(Mutex::new(State {
        service: service.clone(),
        files: HashMap::new(),
        order: VecDeque::new(),
        cursors: HashMap::new(),
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
        if request.id <= previous || request.root.len() != 32 {
            return Err(invalid());
        }
        service.root(&request.root)?;
        previous = request.id;
        if matches!(request.operation, Operation::Watch {}) {
            let root = service.root(&request.root)?;
            return watch(
                stream,
                root,
                generation,
                cancel,
                previous,
                service.jobs.clone(),
            )
            .await;
        }
        let state = state.clone();
        let token = cancel.clone();
        let checked = generation.clone();
        let job_permit = service
            .jobs
            .clone()
            .try_acquire_owned()
            .map_err(|_| invalid())?;
        let result = tokio::time::timeout(
            DEADLINE,
            tokio::task::spawn_blocking(move || {
                let _permit = job_permit;
                let correlation = checked.check(&token)?;
                let result = if matches!(request.operation, Operation::Correlation {}) {
                    Ok((Reply::Correlation { correlation }, None))
                } else {
                    state.lock().map_err(|_| invalid())?.execute(
                        &request.root,
                        request.operation,
                        &token,
                    )
                };
                checked.check(&token)?;
                result
            }),
        )
        .await
        .map_err(|_| invalid())?
        .map_err(|_| invalid())?;
        let (response, data) = match result {
            Ok((result, data)) => (
                rpc::Response {
                    id: previous,
                    result: Some(result),
                    error: None,
                },
                data,
            ),
            Err(error) => (
                rpc::Response {
                    id: previous,
                    result: None,
                    error: Some(error_code(&error)),
                },
                None,
            ),
        };
        write_frame(&mut stream, rpc::RESPONSE, &serde_json::to_vec(&response)?).await?;
        if let Some(data) = data {
            check_generation(&generation, &cancel, &service.jobs).await?;
            let mut payload = previous.to_be_bytes().to_vec();
            payload.extend(data);
            write_frame(&mut stream, rpc::DATA, &payload).await?;
        }
    }
}

struct NativeWatch {
    _watcher: RecommendedWatcher,
    _directories: Vec<File>,
    events: flume::Receiver<notify::Result<notify::Event>>,
    overflow: Arc<AtomicBool>,
    aliases: Vec<(PathBuf, String)>,
    sequence: u64,
}

fn enqueue_event(
    sender: &flume::Sender<notify::Result<notify::Event>>,
    overflow: &AtomicBool,
    event: notify::Result<notify::Event>,
) {
    if event.as_ref().is_ok_and(|event| {
        event.paths.len() > 2 || event.paths.iter().any(|p| p.as_os_str().len() > 4096)
    }) || sender.try_send(event).is_err()
    {
        overflow.store(true, Ordering::Release);
    }
}

fn watch_anchor(file: &File, path: &std::path::Path) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let _ = path;
        #[cfg(target_os = "linux")]
        return PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
        #[cfg(not(target_os = "linux"))]
        return PathBuf::from(format!("/dev/fd/{}", file.as_raw_fd()));
    }
    #[cfg(windows)]
    {
        let _ = file;
        path.to_owned()
    }
}

impl NativeWatch {
    fn open(root: &Root) -> io::Result<Self> {
        // FSEvents watches names rather than retained directory descriptors.
        // Refuse that backend until its native capability binding is qualified.
        if !cfg!(any(windows, target_os = "linux")) {
            return Err(invalid());
        }
        let (sender, events) = flume::bounded(rpc::NOTICE_LIMIT);
        let overflow = Arc::new(AtomicBool::new(false));
        let signal = overflow.clone();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                enqueue_event(&sender, &signal, event);
            })
            .map_err(|_| invalid())?;
        let directory = root.filesystem.directory("/")?;
        let mut directories = vec![directory.try_clone()?.into_std_file()];
        #[cfg(windows)]
        {
            let mut path = PathBuf::new();
            for (index, component) in root.path.components().enumerate() {
                if index >= 64 {
                    return Err(invalid());
                }
                path.push(component.as_os_str());
                if matches!(component, std::path::Component::Prefix(_)) {
                    continue;
                }
                let locked =
                    festerm_windows_security::lock_directory_path_without_delete_sharing(&path)?;
                festerm_windows_security::trusted_private_storage_ancestor(&locked)?;
                directories.push(locked);
            }
            if !festerm_windows_security::same_file_identity(
                directories.last().ok_or_else(invalid)?,
                &directories[0],
            )? {
                return Err(invalid());
            }
        }
        let anchor = watch_anchor(&directories[0], &root.path);
        let mut aliases = vec![(anchor.clone(), String::new())];
        watcher
            .watch(&anchor, RecursiveMode::NonRecursive)
            .map_err(|_| invalid())?;
        // Inspect at most 4096 entries: refuse rather than claim incomplete coverage.
        let mut iterator = directory.entries()?;
        for _ in 0..4096 {
            let Some(entry) = iterator.next() else {
                return Ok(Self {
                    _watcher: watcher,
                    _directories: directories,
                    events,
                    overflow,
                    aliases,
                    sequence: 0,
                });
            };
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(invalid());
            }
            if !kind.is_dir() {
                continue;
            }
            if aliases.len() >= rpc::WATCH_DIRECTORY_LIMIT {
                return Err(invalid());
            }
            let name = entry.file_name();
            let name = name
                .to_str()
                .filter(|name| valid_name(name))
                .ok_or_else(invalid)?;
            let file = directory.open_dir_nofollow_observation(name)?;
            #[cfg(windows)]
            let file = {
                let locked = festerm_windows_security::lock_directory_path_without_delete_sharing(
                    &root.path.join(name),
                )?;
                if !festerm_windows_security::same_file_identity(&locked, &file)? {
                    return Err(invalid());
                }
                locked
            };
            let anchor = watch_anchor(&file, &root.path.join(name));
            watcher
                .watch(&anchor, RecursiveMode::NonRecursive)
                .map_err(|_| invalid())?;
            aliases.push((anchor, format!("/{name}")));
            directories.push(file);
        }
        Err(invalid())
    }

    fn translate(&self, event: notify::Event) -> io::Result<rpc::WatchEvent> {
        if event.need_rescan() || event.paths.is_empty() || event.paths.len() > 2 {
            return Err(invalid());
        }
        if matches!(event.kind, EventKind::Access(_)) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let paths = event
            .paths
            .iter()
            .map(|path| {
                let (alias, prefix) = self
                    .aliases
                    .iter()
                    .rev()
                    .find(|(alias, _)| path.starts_with(alias))
                    .ok_or_else(invalid)?;
                let relative = path.strip_prefix(alias).map_err(|_| invalid())?;
                let mut names = Vec::new();
                for component in relative.components() {
                    let std::path::Component::Normal(name) = component else {
                        return Err(invalid());
                    };
                    let name = name
                        .to_str()
                        .filter(|name| valid_name(name))
                        .ok_or_else(invalid)?;
                    names.push(name);
                }
                if names.is_empty() || names.len() > if prefix.is_empty() { 2 } else { 1 } {
                    return Err(invalid());
                }
                let path = format!("{prefix}/{}", names.join("/"));
                if path.len() > 4096 {
                    return Err(invalid());
                }

                Ok(path)
            })
            .collect::<io::Result<Vec<_>>>()?;
        // A top-level structural change may change our native watch set.
        // Retire instead of silently claiming coverage of a new/replaced directory.
        if paths.iter().any(|p| p[1..].split('/').count() == 1)
            && !matches!(
                event.kind,
                EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_) | ModifyKind::Any)
            )
        {
            return Err(invalid());
        }
        match event.kind {
            EventKind::Create(_) if paths.len() == 1 => Ok(rpc::WatchEvent::Created {
                path: paths[0].clone(),
            }),
            EventKind::Remove(_) if paths.len() == 1 => Ok(rpc::WatchEvent::Removed {
                path: paths[0].clone(),
            }),
            EventKind::Modify(ModifyKind::Name(notify::event::RenameMode::Both))
                if paths.len() == 2 =>
            {
                Ok(rpc::WatchEvent::Renamed {
                    from: paths[0].clone(),
                    to: paths[1].clone(),
                })
            }
            EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_) | ModifyKind::Any)
                if paths.len() == 1 =>
            {
                Ok(rpc::WatchEvent::Modified {
                    path: paths[0].clone(),
                })
            }
            EventKind::Access(_) => Err(io::ErrorKind::WouldBlock.into()),
            _ => Err(invalid()),
        }
    }
}

async fn notice(
    stream: &mut (impl AsyncWrite + Unpin),
    watch: &mut NativeWatch,
    event: rpc::WatchEvent,
) -> io::Result<()> {
    watch.sequence = watch.sequence.checked_add(1).ok_or_else(invalid)?;
    write_frame(
        stream,
        rpc::NOTICE,
        &serde_json::to_vec(&rpc::WatchNotice {
            sequence: watch.sequence.to_string(),
            event,
        })?,
    )
    .await
}

async fn watch(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    root: &Root,
    generation: Generation,
    cancel: CancellationToken,
    id: u32,
    jobs: Arc<Semaphore>,
) -> io::Result<()> {
    let checked = generation.clone();
    let token = cancel.clone();
    let root = root.clone();
    let job_permit = jobs.clone().try_acquire_owned().map_err(|_| invalid())?;
    let setup = tokio::time::timeout(
        DEADLINE,
        tokio::task::spawn_blocking(move || {
            let _permit = job_permit;
            checked.check(&token)?;
            let result = NativeWatch::open(&root);
            checked.check(&token)?;
            result
        }),
    )
    .await
    .map_err(|_| invalid())?
    .map_err(|_| invalid())?;
    check_generation(&generation, &cancel, &jobs).await?;
    let mut native = match setup {
        Ok(native) => native,
        Err(_) => {
            write_frame(
                &mut stream,
                rpc::RESPONSE,
                &serde_json::to_vec(&rpc::Response {
                    id,
                    result: Some(Reply::Watching),
                    error: None,
                })?,
            )
            .await?;
            write_frame(
                &mut stream,
                rpc::NOTICE,
                &serde_json::to_vec(&rpc::WatchNotice {
                    sequence: "1".into(),
                    event: rpc::WatchEvent::RescanRequired,
                })?,
            )
            .await?;
            return Ok(());
        }
    };
    check_generation(&generation, &cancel, &jobs).await?;
    write_frame(
        &mut stream,
        rpc::RESPONSE,
        &serde_json::to_vec(&rpc::Response {
            id,
            result: Some(Reply::Watching),
            error: None,
        })?,
    )
    .await?;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                let _ = notice(&mut stream, &mut native, rpc::WatchEvent::Ended).await;
                return Ok(());
            }
            _ = read_client_frame(&mut stream) => {
                let _ = notice(&mut stream, &mut native, rpc::WatchEvent::Ended).await;
                return Ok(());
            }
            _ = tick.tick() => {}
        }
        if check_generation(&generation, &cancel, &jobs).await.is_err() {
            notice(&mut stream, &mut native, rpc::WatchEvent::Ended).await?;
            return Ok(());
        }
        if native.overflow.load(Ordering::Acquire) {
            notice(&mut stream, &mut native, rpc::WatchEvent::RescanRequired).await?;
            return Ok(());
        }
        let mut events = Vec::new();
        for _ in 0..rpc::NOTICE_LIMIT {
            let Ok(event) = native.events.try_recv() else {
                break;
            };
            let event = event
                .map_err(|_| invalid())
                .and_then(|event| native.translate(event));
            match event {
                Ok(event) => {
                    if !events.contains(&event) {
                        events.push(event);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(_) => {
                    notice(&mut stream, &mut native, rpc::WatchEvent::RescanRequired).await?;
                    return Ok(());
                }
            }
        }
        if native.overflow.load(Ordering::Acquire) {
            notice(&mut stream, &mut native, rpc::WatchEvent::RescanRequired).await?;
            return Ok(());
        }
        for event in events {
            if check_generation(&generation, &cancel, &jobs).await.is_err() {
                notice(&mut stream, &mut native, rpc::WatchEvent::Ended).await?;
                return Ok(());
            }
            notice(&mut stream, &mut native, event).await?;
        }
    }
}

async fn check_generation(
    generation: &Generation,
    cancel: &CancellationToken,
    jobs: &Arc<Semaphore>,
) -> io::Result<()> {
    let generation = generation.clone();
    let cancel = cancel.clone();
    let job_permit = jobs.clone().try_acquire_owned().map_err(|_| invalid())?;
    tokio::time::timeout(
        DEADLINE,
        tokio::task::spawn_blocking(move || {
            let _permit = job_permit;
            generation.check(&cancel)
        }),
    )
    .await
    .map_err(|_| invalid())?
    .map_err(|_| invalid())??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs2::FileExt;
    use std::io::Write;

    #[test]
    fn observation_native_callback_overflow_is_latched_not_silently_dropped() {
        let (sender, receiver) = flume::bounded(rpc::NOTICE_LIMIT);
        let overflow = AtomicBool::new(false);
        for _ in 0..rpc::NOTICE_LIMIT {
            enqueue_event(
                &sender,
                &overflow,
                Ok(notify::Event::new(EventKind::Any).add_path(PathBuf::from("file"))),
            );
        }
        assert!(!overflow.load(Ordering::Acquire));
        enqueue_event(&sender, &overflow, Ok(notify::Event::new(EventKind::Any)));
        assert!(overflow.load(Ordering::Acquire));
        assert_eq!(receiver.len(), rpc::NOTICE_LIMIT);
        let overflow = AtomicBool::new(false);
        enqueue_event(
            &sender,
            &overflow,
            Ok(notify::Event::new(EventKind::Any).add_path(PathBuf::from("x".repeat(4097)))),
        );
        assert!(overflow.load(Ordering::Acquire));
    }

    fn state() -> (PathBuf, State, String) {
        let path = crate::test_support::private_directory("festerm-observation");
        let service = Observation::open(&rpc::Policy {
            roots: vec![path.clone()],
        })
        .unwrap()
        .unwrap();
        let root = service.capabilities(false).roots[0].id.clone();
        let permit = service.slots.clone().try_acquire_owned().unwrap();
        (
            path,
            State {
                service,
                files: HashMap::new(),
                order: VecDeque::new(),
                cursors: HashMap::new(),
                _permit: permit,
            },
            root,
        )
    }
    fn run(state: &mut State, root: &str, operation: Operation) -> (Reply, Option<Vec<u8>>) {
        state
            .execute(root, operation, &CancellationToken::new())
            .unwrap()
    }
    fn stat(state: &mut State, root: &str, path: &str) -> rpc::Metadata {
        let (Reply::Stat { metadata }, None) =
            run(state, root, Operation::Stat { path: path.into() })
        else {
            panic!()
        };
        metadata
    }
    fn read(
        state: &mut State,
        root: &str,
        expected: &rpc::Metadata,
        offset: u64,
        length: u32,
    ) -> (Reply, Option<Vec<u8>>) {
        run(
            state,
            root,
            Operation::Read {
                path: "/large".into(),
                offset: offset.to_string(),
                length,
                expected: expected.clone(),
            },
        )
    }

    #[test]
    fn observation_large_offsets_eof_and_changes_never_return_old_offset_bytes() {
        let (path, mut state, root) = state();
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path.join("large"))
            .unwrap();
        let offset = 61 * 1024 * 1024;
        file.set_len(offset).unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(b"tail").unwrap();
        file.sync_all().unwrap();
        let original = stat(&mut state, &root, "/large");
        let (_, data) = read(&mut state, &root, &original, offset, 4);
        assert_eq!(data.unwrap(), b"tail");
        let (_, data) = read(&mut state, &root, &original, offset + 4, 4);
        assert!(data.unwrap().is_empty());
        file.write_all(b"append").unwrap();
        file.sync_all().unwrap();
        let (Reply::Changed { metadata: appended }, None) =
            read(&mut state, &root, &original, offset, 4)
        else {
            panic!("append released stale bytes")
        };
        assert_eq!(appended.identity, original.identity);
        assert_ne!(appended.revision, original.revision);
        assert_eq!(
            read(&mut state, &root, &appended, offset + 4, 6).1.unwrap(),
            b"append"
        );
        file.set_len(0).unwrap();
        file.sync_all().unwrap();
        let (
            Reply::Changed {
                metadata: truncated,
            },
            None,
        ) = read(&mut state, &root, &appended, offset, 4)
        else {
            panic!("truncation released stale bytes")
        };
        assert_eq!(truncated.size, "0");
        assert!(read(&mut state, &root, &truncated, 0, 4)
            .1
            .unwrap()
            .is_empty());
        drop(file);
        std::fs::rename(path.join("large"), path.join("old")).unwrap();
        std::fs::write(path.join("large"), b"replacement").unwrap();
        let (Reply::Changed { metadata: replaced }, None) =
            read(&mut state, &root, &truncated, 0, 4)
        else {
            panic!("replacement released stale bytes")
        };
        assert_ne!(replaced.identity, truncated.identity);
        assert_eq!(read(&mut state, &root, &replaced, 0, 4).1.unwrap(), b"repl");
        // Same-size rewrites are detected by change time; let the filesystem's
        // timestamp tick advance (about 15.6 ms on Windows) before rewriting.
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::fs::write(path.join("large"), b"same length").unwrap();
        let (Reply::Changed { metadata: changed }, None) = read(&mut state, &root, &replaced, 0, 4)
        else {
            panic!("same-size mutation released stale bytes")
        };
        assert_eq!(changed.size, replaced.size);
        assert_eq!(changed.identity, replaced.identity);
        assert_ne!(changed.revision, replaced.revision);
        let other = stat(&mut state, &root, "/large");
        for n in 0..rpc::FILE_LIMIT {
            let name = format!("item-{n}");
            std::fs::write(path.join(&name), b"").unwrap();
            stat(&mut state, &root, &format!("/{name}"));
        }
        assert_eq!(state.files.len(), rpc::FILE_LIMIT);
        let (Reply::Changed { metadata }, None) = read(&mut state, &root, &other, 0, 4) else {
            panic!("evicted revision authorized bytes")
        };
        assert_ne!(metadata.revision, other.revision);
        drop(state);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn observation_consent_bounds_and_same_handle_native_pages() {
        let (path, mut state, root) = state();
        let policy = rpc::Policy {
            roots: vec![path.clone()],
        };
        let off = crate::files::FileService::configured(None, None).unwrap();
        assert!(off.observation().is_none() && off.filesystem().is_none() && !off.allows_read());
        let only_observation = crate::files::FileService::configured(None, Some(&policy)).unwrap();
        assert!(
            only_observation.observation().is_some()
                && only_observation.filesystem().is_none()
                && !only_observation.allows_read()
        );
        let filesystem = FilePolicy {
            root: path.clone(),
            write: true,
        };
        let only_files = crate::files::FileService::configured(Some(&filesystem), None).unwrap();
        assert!(only_files.observation().is_none() && only_files.filesystem().is_some());
        let both = crate::files::FileService::configured(Some(&filesystem), Some(&policy)).unwrap();
        assert!(both.observation().is_some() && both.filesystem().is_some() && !both.allows_read());
        assert!(crate::files::FileService::new(true).observation().is_none());
        assert!(Observation::open(&rpc::Policy {
            roots: vec![path.clone(); 5]
        })
        .is_err());
        assert!(Observation::open(&rpc::Policy {
            roots: vec![path.clone(); 2]
        })
        .is_err());
        assert!(Observation::open(&rpc::Policy::default())
            .unwrap()
            .is_none());
        std::fs::write(path.join("large"), b"native page").unwrap();
        let expected = stat(&mut state, &root, "/large");
        let (Reply::Directory { cursor }, None) = run(
            &mut state,
            &root,
            Operation::OpenDirectory { path: "/".into() },
        ) else {
            panic!()
        };
        let (Reply::Page { page }, None) = run(
            &mut state,
            &root,
            Operation::ReadDirectory {
                cursor: cursor.clone(),
                limit: 1,
            },
        ) else {
            panic!()
        };
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].metadata, expected);
        assert!(state
            .execute(
                &nonce(),
                Operation::Stat {
                    path: "/large".into()
                },
                &CancellationToken::new()
            )
            .is_err());
        for path in [
            "/../large",
            "//large",
            "/a\\b",
            "/a:stream",
            "/.festerm-transfer-x",
        ] {
            assert!(state
                .execute(
                    &root,
                    Operation::Stat { path: path.into() },
                    &CancellationToken::new()
                )
                .is_err());
        }
        assert!(state
            .execute(
                &root,
                Operation::ReadDirectory {
                    cursor: cursor.clone(),
                    limit: 65
                },
                &CancellationToken::new()
            )
            .is_err());
        assert!(state
            .execute(
                &root,
                Operation::Read {
                    path: "/large".into(),
                    offset: "01".into(),
                    length: 1,
                    expected: expected.clone()
                },
                &CancellationToken::new()
            )
            .is_err());
        assert!(state
            .execute(
                &root,
                Operation::Read {
                    path: "/large".into(),
                    offset: "0".into(),
                    length: rpc::CHUNK_LIMIT as u32 + 1,
                    expected
                },
                &CancellationToken::new()
            )
            .is_err());
        run(&mut state, &root, Operation::CloseDirectory { cursor });
        for _ in 0..rpc::CURSOR_LIMIT {
            run(
                &mut state,
                &root,
                Operation::OpenDirectory { path: "/".into() },
            );
        }
        assert!(state
            .execute(
                &root,
                Operation::OpenDirectory { path: "/".into() },
                &CancellationToken::new()
            )
            .is_err());
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(state
            .execute(&root, Operation::Stat { path: "/".into() }, &cancelled)
            .is_err());
        let mut permits = Vec::new();
        for _ in 1..rpc::STREAM_LIMIT {
            permits.push(state.service.slots.clone().try_acquire_owned().unwrap());
        }
        assert!(state.service.slots.clone().try_acquire_owned().is_err());
        drop((permits, off, only_observation, only_files, both, state));
        std::fs::remove_dir_all(path).unwrap();
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn observation_native_watches_anchor_and_retire_on_coverage_changes() {
        let (path, state, root) = state();
        let directory = path.join("child");
        let child = crate::setup_storage::private_directory(&directory, true).unwrap();
        drop(child);
        std::fs::write(directory.join("file"), b"before").unwrap();
        let service_root = state.service.root(&root).unwrap();
        let native = NativeWatch::open(service_root).unwrap();
        std::fs::write(directory.join("file"), b"after").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut modified = false;
        while std::time::Instant::now() < deadline {
            let Ok(event) = native.events.recv_timeout(Duration::from_millis(100)) else {
                continue;
            };
            if let Ok(rpc::WatchEvent::Modified { path }) = event.and_then(|event| {
                native
                    .translate(event)
                    .map_err(|e| notify::Error::generic(&e.to_string()))
            }) {
                if path == "/child/file" {
                    modified = true;
                    break;
                }
            }
        }
        assert!(modified, "native content-free change was not delivered");
        let structural = notify::Event::new(EventKind::Create(notify::event::CreateKind::Folder))
            .add_path(native.aliases[0].0.join("new"));
        assert!(native.translate(structural).is_err());
        let ambiguous =
            notify::Event::new(EventKind::Any).add_path(native.aliases[0].0.join("file"));
        assert!(native.translate(ambiguous).is_err());
        assert!(native
            .translate(
                notify::Event::new(EventKind::Modify(ModifyKind::Data(
                    notify::event::DataChange::Any
                )))
                .add_path(PathBuf::from("outside"))
            )
            .is_err());
        drop(native);
        for n in 1..rpc::WATCH_DIRECTORY_LIMIT {
            drop(
                crate::setup_storage::private_directory(&path.join(format!("child-{n}")), true)
                    .unwrap(),
            );
        }
        assert!(NativeWatch::open(service_root).is_err());
        drop(state);
        std::fs::remove_dir_all(path).unwrap();
    }

    struct RegistryFixture {
        path: PathBuf,
        generation: Generation,
        lease: File,
    }
    impl RegistryFixture {
        fn new() -> Self {
            let path = crate::test_support::private_directory("festerm-observation-registry");
            let pid = std::process::id();
            let generation = 123456u128;
            let endpoint = path.join(format!("{pid}-{generation}.sock"));
            std::fs::write(&endpoint, b"not a terminal attachment").unwrap();
            let lease = std::fs::OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(path.join(format!("lease-{pid}-{generation}")))
                .unwrap();
            FileExt::lock_exclusive(&lease).unwrap();
            std::fs::write(
                path.join("registry.json"),
                serde_json::to_vec(&serde_json::json!({"sessions":{"observe-test":{
                    "name":"observe-test","pid":pid,"socket":endpoint.to_str().unwrap(),
                    "created_at_unix_ms":generation,"attached":true,"attached_by":"remote",
                    "correlation_id":format!("fesTerm:{pid}:{generation}")
                }}}))
                .unwrap(),
            )
            .unwrap();
            let selected = NativeSessionIdentity {
                name: "observe-test".into(),
                pid,
                created_at_unix_ms: generation,
                endpoint: endpoint.to_str().unwrap().into(),
            };
            Self {
                path: path.clone(),
                generation: Generation {
                    root: path,
                    selected,
                },
                lease,
            }
        }
        fn target(&self) -> Target {
            Target {
                name: self.generation.selected.name.clone(),
                pid: self.generation.selected.pid,
                generation: self.generation.selected.created_at_unix_ms.to_string(),
            }
        }
    }
    impl Drop for RegistryFixture {
        fn drop(&mut self) {
            let _ = FileExt::unlock(&self.lease);
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[tokio::test]
    async fn observation_native_generation_and_stream_contract_refuse_replay_and_expiry() {
        let fixture = RegistryFixture::new();
        let (path, state, root) = state();
        std::fs::write(path.join("large"), b"native bytes").unwrap();
        let bytes = serde_json::to_vec(&rpc::Open {
            version: rpc::VERSION,
            target: fixture.target(),
        })
        .unwrap();
        let (mut client, host) = tokio::io::duplex(128 * 1024);
        let service = state.service.clone();
        let generation = fixture.generation.clone();
        let target = fixture.target();
        let task = tokio::spawn(async move {
            serve_generation(host, service, generation, target, &bytes).await
        });
        let (kind, bytes) = crate::wire::read_frame(&mut client).await.unwrap();
        assert_eq!(kind, rpc::RESPONSE);
        let ready: rpc::Response = serde_json::from_slice(&bytes).unwrap();
        assert!(
            matches!(ready.result, Some(Reply::Ready { capabilities }) if capabilities.correlation)
        );
        let request = rpc::Request {
            id: 1,
            root: root.clone(),
            operation: Operation::Stat {
                path: "/large".into(),
            },
        };
        write_frame(
            &mut client,
            rpc::REQUEST,
            &serde_json::to_vec(&request).unwrap(),
        )
        .await
        .unwrap();
        let (_, bytes) = crate::wire::read_frame(&mut client).await.unwrap();
        let response: rpc::Response = serde_json::from_slice(&bytes).unwrap();
        let Some(Reply::Stat { metadata }) = response.result else {
            panic!()
        };
        let request = rpc::Request {
            id: 2,
            root: root.clone(),
            operation: Operation::Read {
                path: "/large".into(),
                offset: "0".into(),
                length: 6,
                expected: metadata,
            },
        };
        write_frame(
            &mut client,
            rpc::REQUEST,
            &serde_json::to_vec(&request).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            crate::wire::read_frame(&mut client).await.unwrap().0,
            rpc::RESPONSE
        );
        let (kind, bytes) = crate::wire::read_frame(&mut client).await.unwrap();
        assert_eq!(kind, rpc::DATA);
        assert_eq!(&bytes[..4], &2u32.to_be_bytes());
        assert_eq!(&bytes[4..], b"native");
        write_frame(
            &mut client,
            rpc::REQUEST,
            &serde_json::to_vec(&request).unwrap(),
        )
        .await
        .unwrap();
        assert!(task.await.unwrap().is_err());
        let mut stale = fixture.generation.clone();
        stale.selected.created_at_unix_ms += 1;
        assert!(stale.check(&CancellationToken::new()).is_err());
        if !state.service.capabilities(false).watch {
            drop(state);
            std::fs::remove_dir_all(path).unwrap();
            return;
        }
        let (mut client, host) = tokio::io::duplex(128 * 1024);
        let generation = fixture.generation.clone();
        let service = state.service.clone();
        let target = fixture.target();
        let open = serde_json::to_vec(&rpc::Open {
            version: rpc::VERSION,
            target: target.clone(),
        })
        .unwrap();
        let task = tokio::spawn(async move {
            serve_generation(host, service, generation, target, &open).await
        });
        crate::wire::read_frame(&mut client).await.unwrap();
        let request = rpc::Request {
            id: 1,
            root,
            operation: Operation::Watch {},
        };
        write_frame(
            &mut client,
            rpc::REQUEST,
            &serde_json::to_vec(&request).unwrap(),
        )
        .await
        .unwrap();
        crate::wire::read_frame(&mut client).await.unwrap();
        FileExt::unlock(&fixture.lease).unwrap();
        let (kind, bytes) =
            tokio::time::timeout(Duration::from_secs(5), crate::wire::read_frame(&mut client))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(kind, rpc::NOTICE);
        let notice: rpc::WatchNotice = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(notice.event, rpc::WatchEvent::Ended);
        task.await.unwrap().unwrap();
        assert!(fixture.generation.check(&CancellationToken::new()).is_err());
        drop(state);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn observation_native_links_do_not_authorize_objects_or_watch_coverage() {
        let (path, mut state, root) = state();
        let outside = crate::test_support::private_directory("festerm-observation-outside");
        std::fs::write(outside.join("file"), b"outside secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, path.join("escape")).unwrap();
        #[cfg(windows)]
        assert!(std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(path.join("escape"))
            .arg(&outside)
            .output()
            .unwrap()
            .status
            .success());
        for path in ["/escape", "/escape/file"] {
            assert!(state
                .execute(
                    &root,
                    Operation::Stat { path: path.into() },
                    &CancellationToken::new()
                )
                .is_err());
        }
        assert!(NativeWatch::open(state.service.root(&root).unwrap()).is_err());
        let (Reply::Directory { cursor }, None) = run(
            &mut state,
            &root,
            Operation::OpenDirectory { path: "/".into() },
        ) else {
            panic!()
        };
        let (Reply::Page { page }, None) = run(
            &mut state,
            &root,
            Operation::ReadDirectory { cursor, limit: 64 },
        ) else {
            panic!()
        };
        assert!(page.entries.is_empty());
        assert_eq!(page.skipped, 1);
        drop(state);
        #[cfg(windows)]
        std::fs::remove_dir(path.join("escape")).unwrap();
        std::fs::remove_dir_all(path).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }
}
