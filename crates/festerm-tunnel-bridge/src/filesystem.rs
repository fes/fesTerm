//! Explicitly rooted filesystem service; independent of terminal attachment.
use crate::{
    file_protocol::{self as rpc, ErrorCode, Operation, Result as Reply},
    protocol::{invalid, Target},
    wire::{read_client_frame, write_frame, DEADLINE},
};
use cap_fs_ext::DirExt;
use cap_std::fs::Dir;
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

mod locations;
mod reviewed_delete;
use reviewed_delete::ReviewedEntry;

const RESERVED: &str = ".festerm-transfer-";

pub struct Filesystem {
    root: Option<Dir>,
    locations: Option<Vec<locations::Location>>,
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

fn mutation_trusted(directory: &Dir) -> io::Result<()> {
    let file = directory.try_clone()?.into_std_file();
    #[cfg(unix)]
    return festerm_unix_security::qualify_data_mutation_ancestor(&file);
    #[cfg(windows)]
    return festerm_windows_security::qualify_data_mutation_ancestor(&file);
}

impl Filesystem {
    pub fn open(policy: &rpc::Policy) -> io::Result<Arc<Self>> {
        policy.validate()?;
        let locations = policy
            .locations
            .as_ref()
            .map(locations::prepare)
            .transpose()?;
        let directory = if locations.is_none() {
            Some(open_strict_root(&policy.root)?)
        } else {
            None
        };
        let start_in = if let Some(roots) = &locations {
            match &policy.locations.as_ref().expect("prepared locations").start {
                Some(start) => format!(
                    "{}{}",
                    roots[start.root].virtual_root(),
                    if start.path.as_str() == "/" {
                        ""
                    } else {
                        start.path.as_str()
                    }
                ),
                None if roots.len() == 1 => roots[0].virtual_root(),
                None => "/".to_owned(),
            }
        } else {
            policy.initial_directory().to_owned()
        };
        let filesystem = Arc::new(Self {
            root: directory,
            locations,
            start_in,
            display_root: if policy.locations.is_some() {
                None
            } else {
                policy
                    .root
                    .to_str()
                    .and_then(|path| path.to_owned().try_into().ok())
            },
            write: policy.write,
            delete: policy.delete,
            slots: Arc::new(Semaphore::new(2)),
        });
        Ok(filesystem)
    }

    /// Owner preflight and fresh Files opening only; an unavailable initial
    /// directory must never prevent preparation of the allowed root for HOST.
    pub fn validate_start_directory(&self) -> io::Result<()> {
        let mut budget = locations::ReadBudget::new(
            tokio::time::Instant::now() + DEADLINE,
            CancellationToken::new(),
        );
        self.validate_start_directory_with_budget(&mut budget)
    }

    fn validate_start_directory_with_budget(
        &self,
        budget: &mut locations::ReadBudget,
    ) -> io::Result<()> {
        budget.check()?;
        if self.locations.is_some() && self.start_in == "/" {
            return Ok(());
        }
        if self.has_locations() {
            let resolved =
                locations::resolve_read(self, &self.start_in, Some(rpc::Kind::Directory), budget)
                    .map_err(locations::ReadError::into_io)?;
            resolved.proof.validate(self, &resolved.file, budget)?;
        } else {
            self.read_directory(&self.start_in)?;
        }
        budget.check()
    }

    pub fn validate_owner_locations(&self) -> io::Result<()> {
        if let Some(roots) = &self.locations {
            for root in roots {
                root.current()?;
            }
        }
        self.validate_start_directory()
    }
    pub(crate) fn has_locations(&self) -> bool {
        self.locations.is_some()
    }
    fn location_metadata(&self) -> io::Result<Vec<rpc::FileLocation>> {
        self.locations
            .as_ref()
            .map(|roots| roots.iter().map(locations::Location::metadata).collect())
            .ok_or_else(invalid)
    }
    fn read_directory(&self, path: &str) -> io::Result<(Dir, Vec<File>)> {
        if let Some(roots) = &self.locations {
            let (root, relative) = locations::selected(roots, path)?;
            root.directory(Path::new(&relative))
        } else {
            Ok((self.directory(path)?, Vec::new()))
        }
    }
    fn read_file(&self, path: &str) -> io::Result<(File, Vec<File>)> {
        if let Some(roots) = &self.locations {
            let (root, relative) = locations::selected(roots, path)?;
            root.file(Path::new(&relative))
        } else {
            let (parent, name) = self.parent(path)?;
            Ok((open_file(&parent, &name)?, Vec::new()))
        }
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
                if self.locations.is_some() {
                    operations.push("locations".into());
                    operations.push(rpc::READ_LINKS_OPERATION.into());
                }
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
        let (mut directory, relative) = if let Some(roots) = &self.locations {
            let (root, relative) = locations::selected(roots, path)?;
            (root.strict_root()?, relative)
        } else {
            (
                self.root.as_ref().ok_or_else(invalid)?.try_clone()?,
                names(path)?.join("/"),
            )
        };
        if self.has_locations() {
            mutation_trusted(&directory)?;
        } else {
            trusted(&directory)?;
        }
        for name in relative.split('/').filter(|name| !name.is_empty()) {
            directory = directory.open_dir_nofollow(name)?;
            if self.has_locations() {
                mutation_trusted(&directory)?;
            } else {
                trusted(&directory)?;
            }
        }
        Ok(directory)
    }

    fn check_mutation_parent(&self, path: &str, held: &Dir) -> io::Result<()> {
        if !self.has_locations() {
            return Ok(());
        }
        let (fresh, _name) = self.parent(path)?;
        if !locations::same_object(&fresh.into_std_file(), &held.try_clone()?.into_std_file())? {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(())
    }

    pub(crate) fn parent(&self, path: &str) -> io::Result<(Dir, String)> {
        let mut names = names(path)?;
        let name = names.pop().ok_or_else(invalid)?.to_owned();
        let parent = format!("/{}", names.join("/"));
        Ok((self.directory(&parent)?, name))
    }
}

fn open_strict_root(path: &Path) -> io::Result<Dir> {
    open_strict_root_mode(path, false)
}
fn open_strict_root_mode(path: &Path, new_shape: bool) -> io::Result<Dir> {
    let mut components = path.components();
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
        if new_shape {
            mutation_trusted(&directory)?;
        } else {
            trusted(&directory)?;
        }
        let Component::Normal(name) = component else {
            return Err(invalid());
        };
        directory = directory.open_dir_nofollow(name)?;
    }
    if new_shape {
        mutation_trusted(&directory)?;
    } else {
        trusted(&directory)?;
    }
    Ok(directory)
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

/// New Linux mutation leaves must qualify the pinned regular object before
/// data-open. Historical readers and other platforms retain their old path.
fn open_mutation_file(parent: &Dir, name: &str, new_shape: bool) -> io::Result<File> {
    #[cfg(target_os = "linux")]
    if new_shape {
        return festerm_unix_security::open_mutation_file_nofollow(
            &parent.try_clone()?.into_std_file(),
            Path::new(name),
        );
    }
    #[cfg(not(target_os = "linux"))]
    let _ = new_shape;
    open_file(parent, name)
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
    strict_mounts: bool,
}

impl Drop for Upload {
    fn drop(&mut self) {
        if !self.published && (!self.strict_mounts || mutation_trusted(&self.parent).is_ok()) {
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
        readable_proof: Option<locations::ReadProof>,
        _parents: Vec<File>,
    },
    Directory {
        entries: Box<dyn Iterator<Item = io::Result<cap_std::fs::DirEntry>> + Send>,
        path: String,
        held: File,
        readable_proof: Option<locations::ReadProof>,
        read_only: bool,
        _parents: Vec<File>,
    },
    // Retired read resources leave their bounded slot closeable.
    Retired,
    Upload(Upload),
}

impl Handle {
    fn is_readable(&self) -> bool {
        matches!(
            self,
            Self::Read {
                readable_proof: Some(_),
                ..
            } | Self::Directory {
                readable_proof: Some(_),
                ..
            }
        )
    }
    fn retained_descriptors(&self) -> usize {
        match self {
            Self::Read {
                readable_proof: Some(proof),
                ..
            } => proof.retained_cost(),
            Self::Directory {
                readable_proof: Some(proof),
                ..
            } => proof.retained_cost() + 2,
            Self::Read { _parents, .. } => 1 + _parents.len(),
            Self::Directory { _parents, .. } => 3 + _parents.len(),
            Self::EntryReview(_) => 2,
            Self::Upload(upload) => 2 + usize::from(upload.expected.is_some()),
            Self::Retired => 0,
        }
    }
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
        if handle.is_readable() || self.handles.values().any(Handle::is_readable) {
            let held: usize = self
                .handles
                .values()
                .map(Handle::retained_descriptors)
                .sum();
            if held.saturating_add(handle.retained_descriptors()) > locations::READ_DESCRIPTOR_LIMIT
            {
                return Err(ErrorCode::Busy.into());
            }
        }
        let id = self.next.checked_add(1).ok_or(ErrorCode::Busy)?;
        self.next = id;
        self.handles.insert(id, handle);
        Ok(id)
    }

    fn require_readable(&self) -> Outcome<()> {
        if !self.reviewed_operations || !self.filesystem.has_locations() {
            return Err(ErrorCode::Denied.into());
        }
        Ok(())
    }

    fn readable_page(
        &mut self,
        id: u32,
        limit: u16,
        budget: &mut locations::ReadBudget,
    ) -> Outcome<Reply> {
        let mut handle = self.handles.remove(&id).ok_or(ErrorCode::Invalid)?;
        let result = (|| {
            let Handle::Directory {
                entries: iterator,
                path,
                held,
                readable_proof: Some(proof),
                read_only,
                ..
            } = &mut handle
            else {
                return Err(ErrorCode::Invalid.into());
            };
            // The active cursor is still live after removal from the map.
            budget.retain(proof.retained_cost() + 2)?;
            proof.validate(&self.filesystem, held, budget)?;
            let mut entries = Vec::new();
            let mut retained = Vec::new();
            let (mut done, mut skipped) = (false, 0);
            for _ in 0..rpc::PAGE_LIMIT * 4 {
                budget.check()?;
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
                #[cfg(windows)]
                if !kind.is_symlink() {
                    use cap_std::fs::MetadataExt;
                    // Cached no-follow find metadata: never open a cloud or
                    // other unknown reparse entry merely to render its row.
                    if entry.metadata()?.file_attributes() & 0x400 != 0 {
                        entries.push(rpc::ReadableEntry {
                            name: name.into(),
                            kind: rpc::Kind::Unsupported,
                            read_only: true,
                        });
                        continue;
                    }
                }
                if !kind.is_symlink() {
                    // Preserve ordinary no-follow listing: enumerating a name/type
                    // must not require opening that child's data for read access.
                    let kind = if kind.is_dir() {
                        rpc::Kind::Directory
                    } else if kind.is_file() {
                        rpc::Kind::File
                    } else {
                        rpc::Kind::Unsupported
                    };
                    entries.push(rpc::ReadableEntry {
                        name: name.into(),
                        read_only: *read_only || matches!(kind, rpc::Kind::Unsupported),
                        kind,
                    });
                    continue;
                }
                let child = format!("{path}/{name}");
                match locations::resolve_read_child(&self.filesystem, &child, held, budget) {
                    Ok(resolved) => {
                        let value = metadata(&resolved.file)?;
                        entries.push(rpc::ReadableEntry {
                            name: name.into(),
                            kind: value.kind,
                            read_only: *read_only || resolved.read_only,
                        });
                        retained.push(resolved);
                        // Short pages keep at most one retained alias proof.
                        // The next request resumes this same iterator.
                        break;
                    }
                    Err(error) if error.is_unavailable() => {
                        entries.push(rpc::ReadableEntry {
                            name: name.into(),
                            kind: rpc::Kind::Unsupported,
                            read_only: true,
                        });
                    }
                    Err(error) => return Err(error.into_io().into()),
                }
            }
            for resolved in retained {
                resolved
                    .proof
                    .validate(&self.filesystem, &resolved.file, budget)?;
            }
            proof.validate(&self.filesystem, held, budget)?;
            Ok(Reply::ReadableEntries {
                entries,
                done,
                skipped,
            })
        })();
        // Failed cursors cannot resume after an observed fence or budget failure.
        // Their resource-free slot remains compatible with Close cleanup.
        self.handles.insert(
            id,
            if result.is_ok() {
                handle
            } else {
                Handle::Retired
            },
        );
        result
    }

    fn readable_chunk(
        &mut self,
        id: u32,
        offset: u64,
        length: u32,
        budget: &mut locations::ReadBudget,
    ) -> Outcome<(Reply, Option<Vec<u8>>)> {
        let mut handle = self.handles.remove(&id).ok_or(ErrorCode::Invalid)?;
        let result = (|| {
            let Handle::Read {
                file,
                stamp: expected,
                readable_proof: Some(proof),
                ..
            } = &mut handle
            else {
                return Err(ErrorCode::Invalid.into());
            };
            budget.retain(proof.retained_cost())?;
            proof.validate(&self.filesystem, file, budget)?;
            if stamp(file)? != *expected || offset > expected.length {
                return Err(ErrorCode::Conflict.into());
            }
            file.seek(SeekFrom::Start(offset))?;
            let mut bytes = vec![0; u64::from(length).min(expected.length - offset) as usize];
            file.read_exact(&mut bytes)?;
            if stamp(file)? != *expected {
                return Err(ErrorCode::Conflict.into());
            }
            proof.validate(&self.filesystem, file, budget)?;
            Ok((
                Reply::Read {
                    offset: offset.to_string(),
                    length: bytes.len() as u32,
                    eof: offset + bytes.len() as u64 == expected.length,
                },
                Some(bytes),
            ))
        })();
        self.handles.insert(
            id,
            if result.is_ok() {
                handle
            } else {
                Handle::Retired
            },
        );
        result
    }

    #[cfg(test)]
    fn execute(
        &mut self,
        operation: Operation,
        data: Vec<u8>,
        cancel: &CancellationToken,
    ) -> Outcome<(Reply, Option<Vec<u8>>)> {
        self.execute_admitted(
            operation,
            data,
            cancel,
            &self.admission.clone(),
            tokio::time::Instant::now() + DEADLINE,
        )
    }

    fn execute_admitted(
        &mut self,
        operation: Operation,
        data: Vec<u8>,
        cancel: &CancellationToken,
        admission: &Admission,
        deadline: tokio::time::Instant,
    ) -> Outcome<(Reply, Option<Vec<u8>>)> {
        if cancel.is_cancelled() {
            return Err(ErrorCode::Denied.into());
        }
        let mut budget = locations::ReadBudget::new(deadline, cancel.clone());
        let reply = match operation {
            Operation::StartDirectory {} => {
                if !self.reviewed_operations {
                    return Err(ErrorCode::Denied.into());
                }
                self.filesystem
                    .validate_start_directory_with_budget(&mut budget)?;
                Reply::StartDirectory {
                    path: self
                        .filesystem
                        .start_in
                        .clone()
                        .try_into()
                        .map_err(|_| ErrorCode::Invalid)?,
                }
            }
            Operation::Locations {} => {
                if !self.reviewed_operations || !self.filesystem.has_locations() {
                    return Err(ErrorCode::Denied.into());
                }
                Reply::Locations {
                    locations: self.filesystem.location_metadata()?,
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
                reviewed_delete::remove(entry, admission, cancel, &self.filesystem)?;
                Reply::Deleted
            }
            Operation::Ping {} => Reply::Pong,
            Operation::Close { handle } => {
                self.handles.remove(&handle).ok_or(ErrorCode::Invalid)?;
                Reply::Closed
            }
            Operation::StatReadable { path } => {
                self.require_readable()?;
                let resolved = locations::resolve_read(&self.filesystem, &path, None, &mut budget)
                    .map_err(locations::ReadError::into_io)?;
                let value = metadata(&resolved.file)?;
                resolved
                    .proof
                    .validate(&self.filesystem, &resolved.file, &mut budget)?;
                Reply::ReadableStat {
                    metadata: value,
                    read_only: resolved.read_only,
                }
            }
            Operation::OpenReadableDirectory { path } => {
                self.require_readable()?;
                let resolved = locations::resolve_read(
                    &self.filesystem,
                    &path,
                    Some(rpc::Kind::Directory),
                    &mut budget,
                )
                .map_err(locations::ReadError::into_io)?;
                let value = metadata(&resolved.file)?;
                budget.check_live(3)?;
                let directory = Dir::from_std_file(resolved.file.try_clone()?);
                let entries = Box::new(directory.entries()?);
                drop(directory);
                budget.retain(2)?;
                resolved
                    .proof
                    .validate(&self.filesystem, &resolved.file, &mut budget)?;
                Reply::ReadableDirectory {
                    metadata: value,
                    read_only: resolved.read_only,
                    handle: self.insert(Handle::Directory {
                        path,
                        entries,
                        held: resolved.file,
                        readable_proof: Some(resolved.proof),
                        read_only: resolved.read_only,
                        _parents: Vec::new(),
                    })?,
                }
            }
            Operation::OpenReadableFile { path } => {
                self.require_readable()?;
                let resolved = locations::resolve_read(
                    &self.filesystem,
                    &path,
                    Some(rpc::Kind::File),
                    &mut budget,
                )
                .map_err(locations::ReadError::into_io)?;
                let value = metadata(&resolved.file)?;
                let captured = stamp(&resolved.file)?;
                resolved
                    .proof
                    .validate(&self.filesystem, &resolved.file, &mut budget)?;
                Reply::File {
                    metadata: value,
                    handle: self.insert(Handle::Read {
                        path,
                        file: resolved.file,
                        stamp: captured,
                        readable_proof: Some(resolved.proof),
                        _parents: Vec::new(),
                    })?,
                }
            }
            Operation::Stat { path } => {
                let (file, _parents) = match self.filesystem.read_directory(&path) {
                    Ok((directory, parents)) => (directory.into_std_file(), parents),
                    Err(_) => self.filesystem.read_file(&path)?,
                };
                let value = metadata(&file)?;
                if self.filesystem.has_locations() {
                    if file.metadata()?.is_dir() {
                        locations::check_directory(&self.filesystem, &path, &file)?;
                    } else {
                        locations::check_file(&self.filesystem, &path, &file)?;
                    }
                }
                Reply::Stat { metadata: value }
            }
            Operation::OpenDirectory { path } => {
                let (directory, parents) = self.filesystem.read_directory(&path)?;
                let held = directory.try_clone()?.into_std_file();
                if self.filesystem.has_locations() {
                    locations::check_directory(&self.filesystem, &path, &held)?;
                }
                Reply::Directory {
                    handle: self.insert(Handle::Directory {
                        entries: Box::new(directory.entries()?),
                        path,
                        held,
                        readable_proof: None,
                        read_only: false,
                        _parents: parents,
                    })?,
                }
            }
            Operation::ReadDirectory { handle, limit } => {
                if limit == 0 || limit > rpc::PAGE_LIMIT {
                    return Err(ErrorCode::Invalid.into());
                }
                if matches!(
                    self.handles.get(&handle),
                    Some(Handle::Directory {
                        readable_proof: Some(_),
                        ..
                    })
                ) {
                    self.require_readable()?;
                    self.readable_page(handle, limit, &mut budget)?
                } else {
                    let Some(Handle::Directory {
                        entries: iterator,
                        path,
                        held,
                        ..
                    }) = self.handles.get_mut(&handle)
                    else {
                        return Err(ErrorCode::Invalid.into());
                    };
                    if self.filesystem.has_locations() {
                        locations::check_directory(&self.filesystem, path, held)?;
                    }
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
                    if self.filesystem.has_locations() {
                        locations::check_directory(&self.filesystem, path, held)?;
                    }
                    Reply::Entries {
                        entries,
                        done,
                        skipped,
                    }
                }
            }
            Operation::OpenRead { path } => {
                let (file, parents) = self.filesystem.read_file(&path)?;
                let value = metadata(&file)?;
                let captured = stamp(&file)?;
                if self.filesystem.has_locations() {
                    locations::check_file(&self.filesystem, &path, &file)?;
                }
                let handle = self.insert(Handle::Read {
                    path,
                    file,
                    stamp: captured,
                    readable_proof: None,
                    _parents: parents,
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
                if matches!(
                    self.handles.get(&handle),
                    Some(Handle::Read {
                        readable_proof: Some(_),
                        ..
                    })
                ) {
                    self.require_readable()?;
                    return self.readable_chunk(handle, offset, length, &mut budget);
                }
                let Some(Handle::Read {
                    file,
                    path,
                    stamp: expected,
                    ..
                }) = self.handles.get_mut(&handle)
                else {
                    return Err(ErrorCode::Invalid.into());
                };
                if self.filesystem.has_locations() {
                    locations::check_file(&self.filesystem, path, file)?;
                }
                if stamp(file)? != *expected || offset > expected.length {
                    return Err(ErrorCode::Conflict.into());
                }
                file.seek(SeekFrom::Start(offset))?;
                let mut bytes = vec![0; (u64::from(length).min(expected.length - offset)) as usize];
                file.read_exact(&mut bytes)?;
                if stamp(file)? != *expected {
                    return Err(ErrorCode::Conflict.into());
                }
                if self.filesystem.has_locations() {
                    locations::check_file(&self.filesystem, path, file)?;
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
                self.filesystem.check_mutation_parent(&path, &parent)?;
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
                            readable_proof: None,
                            ..
                        }) = self.handles.get(&handle)
                        else {
                            return Err(ErrorCode::Invalid.into());
                        };
                        if original != &path {
                            return Err(ErrorCode::Invalid.into());
                        }
                        let current =
                            open_mutation_file(&parent, &target, self.filesystem.has_locations())?;
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
                self.filesystem.check_mutation_parent(&path, &parent)?;
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
                    strict_mounts: self.filesystem.has_locations(),
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
                self.filesystem
                    .check_mutation_parent(&upload.path, &upload.parent)?;
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
                self.filesystem
                    .check_mutation_parent(&upload.path, &upload.parent)?;
                upload.file.sync_all()?;
                // Final publication admission is distinct from the request job.
                // The callback releases protected policy before native commit.
                admission(AdmissionStage::Publication)?;
                finish(&mut upload, cancel, |upload| {
                    self.filesystem
                        .check_mutation_parent(&upload.path, &upload.parent)
                })?;
                Reply::Finished {
                    size: upload.size.to_string(),
                }
            }
        };
        Ok((reply, None))
    }
}

fn finish(
    upload: &mut Upload,
    cancel: &CancellationToken,
    check: impl Fn(&Upload) -> io::Result<()>,
) -> Outcome<()> {
    if cancel.is_cancelled() {
        return Err(ErrorCode::Denied.into());
    }
    let Some(expected) = upload.expected.take() else {
        check(upload)?;
        publish(
            &upload.parent,
            &upload.temporary,
            &upload.file,
            &upload.target,
        )?;
        upload.published = true;
        return Ok(());
    };
    let current = open_mutation_file(&upload.parent, &upload.target, upload.strict_mounts)?;
    if !same(&current, &expected.file)? || stamp(&current)? != expected.stamp {
        return Err(ErrorCode::Conflict.into());
    }
    #[cfg(windows)]
    let current = festerm_windows_security::open_file_no_reparse_for_move(
        &upload.parent.try_clone()?.into_std_file(),
        Path::new(&upload.target),
    )?;
    let backup = format!("{RESERVED}backup-{}", uuid::Uuid::new_v4().simple());
    check(upload)?;
    publish(&upload.parent, &upload.target, &current, &backup)?;
    let result = (|| -> Outcome<()> {
        let captured = open_mutation_file(&upload.parent, &backup, upload.strict_mounts)?;
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
        check(upload)?;
        publish(
            &upload.parent,
            &upload.temporary,
            &upload.file,
            &upload.target,
        )?;
        upload.published = true;
        check(upload).map_err(|_| recovery(upload, &backup))?;
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
        // Restoration is against the exact held capture; do not publish new
        // data after containment loss. Keep existing no-replace recovery.
        if upload.strict_mounts && mutation_trusted(&upload.parent).is_err()
            || publish(&upload.parent, &backup, &current, &upload.target).is_err()
        {
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
    if filesystem.has_locations() || open.version != rpc::VERSION || open.target != expected {
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
    if filesystem.has_locations() {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
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
                    session.execute_admitted(request.operation, data, &token, &admission, deadline)
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
            locations: None,
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
            Reply::File { handle, .. }
            | Reply::Upload { handle }
            | Reply::Directory { handle }
            | Reply::ReadableDirectory { handle, .. } => handle,
            _ => panic!("missing handle"),
        }
    }

    fn run(session: &mut Session, operation: Operation) -> Reply {
        session
            .execute(operation, Vec::new(), &CancellationToken::new())
            .unwrap_or_else(|error| panic!("{:?}", error.code))
            .0
    }

    fn locations_session(policy: rpc::Policy) -> Session {
        let filesystem = Filesystem::open(&policy).unwrap();
        let permit = filesystem.slots.clone().try_acquire_owned().unwrap();
        Session {
            reviewed_operations: true,
            admission: Arc::new(|_| Ok(())),
            filesystem,
            handles: HashMap::new(),
            next: 0,
            _permit: Arc::new(permit),
        }
    }
    #[cfg(unix)]
    fn mutation_fixture() -> (tempfile::TempDir, Session, String) {
        let base = tempfile::Builder::new()
            .prefix(".festerm-mutation-fence-")
            .tempdir_in(festerm_unix_security::current_user_home().unwrap())
            .unwrap();
        let allowed = base.path().join("allowed");
        std::fs::create_dir(&allowed).unwrap();
        std::fs::create_dir(allowed.join("inside")).unwrap();
        let session = locations_session(rpc::Policy {
            root: allowed.clone(),
            start_in: None,
            locations: Some(rpc::AllowedLocations {
                roots: vec![allowed],
                start: None,
            }),
            write: true,
            delete: true,
        });
        let selector = session.filesystem.location_metadata().unwrap()[0].virtual_root();
        (base, session, selector)
    }
    #[test]
    fn readable_links_requests_refuse_without_explicit_locations() {
        let (path, mut session) = session(true);
        for operation in [
            Operation::StatReadable { path: "/".into() },
            Operation::OpenReadableDirectory { path: "/".into() },
            Operation::OpenReadableFile {
                path: "/file".into(),
            },
        ] {
            assert!(matches!(
                session.execute(operation, vec![], &CancellationToken::new()),
                Err(Failure {
                    code: ErrorCode::Denied,
                    ..
                })
            ));
            assert!(session.handles.is_empty());
        }
        drop(session);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[cfg(unix)]
    fn readable_fixture() -> Option<(tempfile::TempDir, Session, String)> {
        let (base, session, selector) = mutation_fixture();
        if !session.filesystem.location_metadata().unwrap()[0].available {
            let slash =
                festerm_unix_security::open_absolute_directory_read_nofollow(Path::new("/"))
                    .unwrap();
            assert_eq!(
                festerm_unix_security::open_contained_data_directory(&slash, Path::new(""))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::Unsupported
            );
            eprintln!("readable-links native fixture unavailable on unsupported native floor");
            return None;
        }
        Some((base, session, selector))
    }

    #[test]
    #[cfg(unix)]
    fn readable_links_same_inode_cutover_never_supplies_replacement_proof() {
        use std::os::unix::fs::symlink;
        let Some((base, mut session, selector)) = readable_fixture() else {
            return;
        };
        let directory = base.path().join("allowed/inside");
        std::fs::write(directory.join("target"), b"original").unwrap();
        symlink("target", directory.join("alias")).unwrap();
        let alias = format!("{selector}/inside/alias");
        let read = handle(run(
            &mut session,
            Operation::OpenReadableFile {
                path: alias.clone(),
            },
        ));
        std::fs::remove_file(directory.join("alias")).unwrap();
        std::fs::hard_link(directory.join("target"), directory.join("alias")).unwrap();
        assert!(session
            .execute(
                Operation::BeginUpload {
                    path: alias,
                    size: "3".into(),
                    expected: Some(read)
                },
                vec![],
                &CancellationToken::new()
            )
            .is_err());
        run(&mut session, Operation::Close { handle: read });
        if session.filesystem.location_metadata().unwrap()[0].mutations_available {
            let target = format!("{selector}/inside/target");
            let strict = handle(run(
                &mut session,
                Operation::OpenRead {
                    path: target.clone(),
                },
            ));
            let upload = handle(run(
                &mut session,
                Operation::BeginUpload {
                    path: target,
                    size: "3".into(),
                    expected: Some(strict),
                },
            ));
            run(&mut session, Operation::Close { handle: upload });
            run(&mut session, Operation::Close { handle: strict });
        }
        assert_eq!(
            std::fs::read(directory.join("target")).unwrap(),
            b"original"
        );
        assert_eq!(std::fs::read(directory.join("alias")).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 2);
    }

    #[test]
    #[cfg(unix)]
    fn readable_links_directory_rows_and_descendants_are_read_only() {
        use std::os::unix::fs::symlink;
        let Some((base, mut session, selector)) = readable_fixture() else {
            return;
        };
        let allowed = base.path().join("allowed");
        std::fs::write(allowed.join("inside/target"), b"original").unwrap();
        symlink("inside", allowed.join("alias")).unwrap();
        symlink("inside", allowed.join("alias-two")).unwrap();
        let root = handle(run(
            &mut session,
            Operation::OpenReadableDirectory {
                path: selector.clone(),
            },
        ));
        let mut entries = Vec::new();
        let mut pages = 0;
        loop {
            let Reply::ReadableEntries {
                entries: page,
                done,
                ..
            } = run(
                &mut session,
                Operation::ReadDirectory {
                    handle: root,
                    limit: rpc::PAGE_LIMIT,
                },
            )
            else {
                panic!("tagged page");
            };
            pages += 1;
            assert!(pages <= 4, "short pages must advance the same cursor");
            assert!(page.iter().filter(|entry| entry.read_only).count() <= 1);
            entries.extend(page);
            if done {
                break;
            }
        }
        assert!(pages >= 2);
        assert_eq!(entries.len(), 3);
        assert!(entries.iter().any(|entry| entry.name == "alias"
            && matches!(entry.kind, rpc::Kind::Directory)
            && entry.read_only));
        assert!(entries.iter().any(|entry| entry.name == "alias-two"
            && matches!(entry.kind, rpc::Kind::Directory)
            && entry.read_only));
        assert!(entries.iter().any(|entry| entry.name == "inside"
            && matches!(entry.kind, rpc::Kind::Directory)
            && !entry.read_only));
        run(&mut session, Operation::Close { handle: root });
        let alias = format!("{selector}/alias");
        assert!(session
            .execute(
                Operation::OpenDirectory {
                    path: alias.clone()
                },
                vec![],
                &CancellationToken::new()
            )
            .is_err());
        assert!(session
            .execute(
                Operation::OpenRead {
                    path: format!("{alias}/target")
                },
                vec![],
                &CancellationToken::new()
            )
            .is_err());
        Arc::get_mut(&mut session.filesystem).unwrap().start_in = alias.clone();
        session.filesystem.validate_owner_locations().unwrap();
        assert!(
            matches!(run(&mut session, Operation::StartDirectory {}), Reply::StartDirectory { path } if path.as_str() == alias)
        );
        let Reply::ReadableDirectory {
            handle: cursor,
            metadata,
            read_only,
        } = run(
            &mut session,
            Operation::OpenReadableDirectory {
                path: alias.clone(),
            },
        )
        else {
            panic!("tagged directory");
        };
        assert!(read_only);
        assert!(matches!(metadata.kind, rpc::Kind::Directory));
        let Reply::ReadableEntries { entries, .. } = run(
            &mut session,
            Operation::ReadDirectory {
                handle: cursor,
                limit: rpc::PAGE_LIMIT,
            },
        ) else {
            panic!("tagged page");
        };
        assert_eq!(entries.len(), 1);
        assert!(entries[0].read_only);
        assert!(matches!(entries[0].kind, rpc::Kind::File));
        assert!(session
            .execute(
                Operation::Mkdir {
                    path: format!("{alias}/refused")
                },
                vec![],
                &CancellationToken::new()
            )
            .is_err());
        assert!(session
            .execute(
                Operation::BeginUpload {
                    path: format!("{alias}/refused"),
                    size: "0".into(),
                    expected: None
                },
                vec![],
                &CancellationToken::new()
            )
            .is_err());
        assert!(session
            .execute(
                Operation::ReviewEntry {
                    path: format!("{alias}/target"),
                    purpose: rpc::ReviewPurpose::Delete
                },
                vec![],
                &CancellationToken::new()
            )
            .is_err());
        run(&mut session, Operation::Close { handle: cursor });
        assert_eq!(
            std::fs::read_dir(allowed.join("inside")).unwrap().count(),
            1
        );
        assert_eq!(
            std::fs::read(allowed.join("inside/target")).unwrap(),
            b"original"
        );
    }

    #[test]
    #[cfg(unix)]
    fn readable_links_runtime_start_keeps_original_deadline_and_cancellation() {
        let Some((_base, session, _selector)) = readable_fixture() else {
            return;
        };
        let cancel = CancellationToken::new();
        let mut session = session;
        let admission = session.admission.clone();
        assert!(session
            .execute_admitted(
                Operation::StartDirectory {},
                vec![],
                &cancel,
                &admission,
                tokio::time::Instant::now() - std::time::Duration::from_secs(1)
            )
            .is_err());
        let mut budget =
            locations::ReadBudget::new(tokio::time::Instant::now() + DEADLINE, cancel.clone());
        cancel.cancel();
        assert_eq!(
            session
                .filesystem
                .validate_start_directory_with_budget(&mut budget)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        // A separate owner preflight has its own fresh budget; expiry is not
        // persisted and never replaces the runtime command's deadline.
        session.filesystem.validate_start_directory().unwrap();
        assert!(matches!(
            run(&mut session, Operation::StartDirectory {}),
            Reply::StartDirectory { .. }
        ));
        assert!(session.handles.is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn readable_links_descriptor_admission_refuses_before_handle_limit_and_releases() {
        use std::os::unix::fs::symlink;
        let Some((base, mut session, selector)) = readable_fixture() else {
            return;
        };
        let allowed = base.path().join("allowed");
        std::fs::write(allowed.join("inside/target"), b"original").unwrap();
        symlink("inside/target", allowed.join("second")).unwrap();
        symlink("second", allowed.join("first")).unwrap();
        let mut held = Vec::new();
        loop {
            match session.execute(
                Operation::OpenReadableFile {
                    path: format!("{selector}/first"),
                },
                vec![],
                &CancellationToken::new(),
            ) {
                Ok((Reply::File { handle, .. }, _)) => {
                    held.push(handle);
                    assert!(
                        held.len() < rpc::HANDLE_LIMIT,
                        "descriptor cap must bind before slot count"
                    );
                }
                Err(Failure {
                    code: ErrorCode::Busy,
                    ..
                }) => break,
                _ => panic!("expected bounded descriptor admission"),
            }
        }
        assert!(!held.is_empty());
        for handle in held {
            run(&mut session, Operation::Close { handle });
        }
        assert!(session.handles.is_empty());
        let fresh = handle(run(
            &mut session,
            Operation::OpenReadableFile {
                path: format!("{selector}/first"),
            },
        ));
        run(&mut session, Operation::Close { handle: fresh });
        assert_eq!(
            std::fs::read(allowed.join("inside/target")).unwrap(),
            b"original"
        );
    }

    #[test]
    #[cfg(unix)]
    fn readable_links_ordinary_unreadable_file_does_not_block_parent_listing() {
        use std::os::unix::fs::PermissionsExt;
        let Some((base, mut session, selector)) = readable_fixture() else {
            return;
        };
        if festerm_unix_security::current_user_id().unwrap() == 0 {
            eprintln!("ordinary unreadable file control requires a non-root OS account");
            return;
        }
        let directory = base.path().join("allowed/inside");
        let secret = directory.join("unreadable");
        std::fs::write(&secret, b"unchanged").unwrap();
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert_eq!(
            File::open(&secret).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        let path = format!("{selector}/inside");
        let cursor = handle(run(
            &mut session,
            Operation::OpenReadableDirectory { path: path.clone() },
        ));
        let Reply::ReadableEntries { entries, .. } = run(
            &mut session,
            Operation::ReadDirectory {
                handle: cursor,
                limit: rpc::PAGE_LIMIT,
            },
        ) else {
            panic!("tagged page");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "unreadable");
        assert!(matches!(entries[0].kind, rpc::Kind::File));
        assert!(session
            .execute(
                Operation::OpenReadableFile {
                    path: format!("{path}/unreadable")
                },
                vec![],
                &CancellationToken::new()
            )
            .is_err());
        assert_eq!(session.handles.len(), 1);
        run(&mut session, Operation::Close { handle: cursor });
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(std::fs::read(secret).unwrap(), b"unchanged");
    }

    #[test]
    #[cfg(unix)]
    fn readable_links_expired_cursor_releases_resources_and_remains_closeable() {
        let Some((_base, mut session, selector)) = readable_fixture() else {
            return;
        };
        let cursor = handle(run(
            &mut session,
            Operation::OpenReadableDirectory { path: selector },
        ));
        let cancel = CancellationToken::new();
        let admission = session.admission.clone();
        assert!(session
            .execute_admitted(
                Operation::ReadDirectory {
                    handle: cursor,
                    limit: rpc::PAGE_LIMIT
                },
                vec![],
                &cancel,
                &admission,
                tokio::time::Instant::now() - std::time::Duration::from_secs(1)
            )
            .is_err());
        assert!(matches!(
            session.handles.get(&cursor),
            Some(Handle::Retired)
        ));
        assert!(matches!(
            run(&mut session, Operation::Close { handle: cursor }),
            Reply::Closed
        ));
        assert!(session.handles.is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn allowed_locations_mutation_leaves_share_qualified_review_and_replacement_route() {
        use std::os::unix::fs::symlink;
        let (base, mut session, selector) = mutation_fixture();
        let directory = base.path().join("allowed/inside");
        let target = directory.join("target");
        std::fs::write(&target, b"old").unwrap();
        let path = format!("{selector}/inside/target");
        if !session.filesystem.location_metadata().unwrap()[0].mutations_available {
            assert!(session
                .execute(
                    Operation::ReviewEntry {
                        path,
                        purpose: rpc::ReviewPurpose::Delete
                    },
                    vec![],
                    &CancellationToken::new()
                )
                .is_err());
            assert_eq!(std::fs::read(&target).unwrap(), b"old");
            return;
        }
        let review = match run(
            &mut session,
            Operation::ReviewEntry {
                path: path.clone(),
                purpose: rpc::ReviewPurpose::Delete,
            },
        ) {
            Reply::EntryReview { handle, metadata } => {
                assert_eq!(metadata.size, "3");
                handle
            }
            _ => panic!("exact review"),
        };
        run(&mut session, Operation::Close { handle: review });
        let expected = handle(run(
            &mut session,
            Operation::OpenRead { path: path.clone() },
        ));
        let upload = handle(run(
            &mut session,
            Operation::BeginUpload {
                path: path.clone(),
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
        run(&mut session, Operation::FinishUpload { handle: upload });
        run(&mut session, Operation::Close { handle: expected });
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        let expected = handle(run(
            &mut session,
            Operation::OpenRead { path: path.clone() },
        ));
        std::fs::rename(&target, directory.join("preserved")).unwrap();
        symlink(directory.join("preserved"), &target).unwrap();
        let before = session.handles.len();
        assert!(session
            .execute(
                Operation::ReviewEntry {
                    path: path.clone(),
                    purpose: rpc::ReviewPurpose::Delete
                },
                vec![],
                &CancellationToken::new()
            )
            .is_err());
        assert_eq!(session.handles.len(), before);
        assert!(session
            .execute(
                Operation::BeginUpload {
                    path: path.clone(),
                    size: "3".into(),
                    expected: Some(expected)
                },
                vec![],
                &CancellationToken::new()
            )
            .is_err());
        assert!(std::fs::symlink_metadata(&target)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read(directory.join("preserved")).unwrap(), b"new");
        run(&mut session, Operation::Close { handle: expected });
        std::fs::remove_file(&target).unwrap();
        std::fs::rename(directory.join("preserved"), &target).unwrap();
        let expected = handle(run(
            &mut session,
            Operation::OpenRead { path: path.clone() },
        ));
        let upload = handle(run(
            &mut session,
            Operation::BeginUpload {
                path,
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
                b"bad".to_vec(),
                &CancellationToken::new(),
            )
            .unwrap();
        std::fs::rename(&target, directory.join("preserved")).unwrap();
        symlink(directory.join("preserved"), &target).unwrap();
        assert!(session
            .execute(
                Operation::FinishUpload { handle: upload },
                vec![],
                &CancellationToken::new()
            )
            .is_err());
        assert!(std::fs::symlink_metadata(&target)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read(directory.join("preserved")).unwrap(), b"new");
        assert_eq!(
            std::fs::read_dir(&directory).unwrap().count(),
            2,
            "no capture or staging name after refusal"
        );
        run(&mut session, Operation::Close { handle: expected });
    }

    #[test]
    #[cfg(unix)]
    fn allowed_locations_retained_upload_refuses_parent_and_root_moves_between_rpcs() {
        for move_root in [false, true] {
            for finish in [false, true] {
                let (base, mut session, selector) = mutation_fixture();
                let path = format!("{selector}/inside/new");
                if !session.filesystem.location_metadata().unwrap()[0].mutations_available {
                    assert!(session
                        .execute(
                            Operation::BeginUpload {
                                path,
                                size: "3".into(),
                                expected: None
                            },
                            vec![],
                            &CancellationToken::new()
                        )
                        .is_err());
                    assert_eq!(
                        std::fs::read_dir(base.path().join("allowed/inside"))
                            .unwrap()
                            .count(),
                        0
                    );
                    eprintln!(
                        "new strict mutation deployment unavailable; refused without staging"
                    );
                    continue;
                }
                let upload = handle(run(
                    &mut session,
                    Operation::BeginUpload {
                        path,
                        size: "3".into(),
                        expected: None,
                    },
                ));
                if finish {
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
                }
                let from = base.path().join(if move_root {
                    "allowed"
                } else {
                    "allowed/inside"
                });
                let moved = base.path().join("moved");
                std::fs::rename(from, &moved).unwrap();
                let parent = if move_root {
                    moved.join("inside")
                } else {
                    moved
                };
                let result = if finish {
                    session.execute(
                        Operation::FinishUpload { handle: upload },
                        vec![],
                        &CancellationToken::new(),
                    )
                } else {
                    session.execute(
                        Operation::Write {
                            handle: upload,
                            offset: "0".into(),
                            length: 3,
                        },
                        b"new".to_vec(),
                        &CancellationToken::new(),
                    )
                };
                assert!(result.is_err());
                assert!(!parent.join("new").exists());
                if !finish {
                    let stage = std::fs::read_dir(&parent)
                        .unwrap()
                        .next()
                        .unwrap()
                        .unwrap()
                        .path();
                    assert!(std::fs::read(stage).unwrap().is_empty());
                    run(&mut session, Operation::Close { handle: upload });
                }
                assert_eq!(
                    std::fs::read_dir(parent).unwrap().count(),
                    0,
                    "exact owned staging cleanup survives containment refusal"
                );
            }
        }
    }
    #[test]
    #[cfg(unix)]
    fn allowed_locations_reviewed_delete_refuses_moves_and_restores_after_capture() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for phase in [0, 1, 2] {
            let (base, mut session, selector) = mutation_fixture();
            let original = base.path().join("allowed/inside/original");
            std::fs::write(&original, b"original").unwrap();
            let path = format!("{selector}/inside/original");
            if !session.filesystem.location_metadata().unwrap()[0].mutations_available {
                assert!(session
                    .execute(
                        Operation::ReviewEntry {
                            path,
                            purpose: rpc::ReviewPurpose::Delete
                        },
                        vec![],
                        &CancellationToken::new()
                    )
                    .is_err());
                assert_eq!(std::fs::read(original).unwrap(), b"original");
                continue;
            }
            let id = match run(
                &mut session,
                Operation::ReviewEntry {
                    path,
                    purpose: rpc::ReviewPurpose::Delete,
                },
            ) {
                Reply::EntryReview { handle, .. } => handle,
                _ => panic!("review"),
            };
            let moved = base.path().join("moved");
            if phase < 2 {
                std::fs::rename(
                    base.path().join(if phase == 0 {
                        "allowed/inside"
                    } else {
                        "allowed"
                    }),
                    &moved,
                )
                .unwrap();
            } else {
                let calls = Arc::new(AtomicUsize::new(0));
                let checked = calls.clone();
                let from = base.path().join("allowed/inside");
                let destination = moved.clone();
                session.admission = Arc::new(move |stage| {
                    if matches!(stage, AdmissionStage::Publication)
                        && checked.fetch_add(1, Ordering::SeqCst) == 1
                    {
                        std::fs::rename(&from, &destination)?;
                    }
                    Ok(())
                });
            }
            assert!(session
                .execute(
                    Operation::Delete { review: id },
                    vec![],
                    &CancellationToken::new()
                )
                .is_err());
            let parent = if phase == 1 {
                moved.join("inside")
            } else {
                moved
            };
            assert_eq!(std::fs::read(parent.join("original")).unwrap(), b"original");
            assert_eq!(
                std::fs::read_dir(parent).unwrap().count(),
                1,
                "no retained capture after bounded restore"
            );
        }
    }
    #[test]
    #[cfg(target_os = "macos")]
    fn allowed_locations_ordinary_read_never_grants_unqualified_strict_mutations() {
        let base = crate::test_support::private_directory("festerm-noowners-read");
        std::fs::write(base.join("original"), b"original").unwrap();
        let mut session = locations_session(rpc::Policy {
            root: base.clone(),
            start_in: None,
            locations: Some(rpc::AllowedLocations {
                roots: vec![base.clone()],
                start: None,
            }),
            write: true,
            delete: true,
        });
        let root = session.filesystem.location_metadata().unwrap().remove(0);
        if root.available && !root.mutations_available {
            let native =
                festerm_unix_security::open_absolute_directory_read_nofollow(&base).unwrap();
            let ownership =
                festerm_unix_security::ownership_enforcing_data_filesystem(&native).unwrap();
            eprintln!(
                "ordinary data fixture ownership_enforced={ownership}; strict mutations refused"
            );
            let path = format!("{}/original", root.virtual_root());
            let read = handle(run(
                &mut session,
                Operation::OpenRead { path: path.clone() },
            ));
            let (_, bytes) = session
                .execute(
                    Operation::Read {
                        handle: read,
                        offset: "0".into(),
                        length: 32,
                    },
                    vec![],
                    &CancellationToken::new(),
                )
                .unwrap();
            assert_eq!(bytes.unwrap(), b"original");
            run(&mut session, Operation::Close { handle: read });
            for op in [
                Operation::Mkdir {
                    path: format!("{}/new", root.virtual_root()),
                },
                Operation::BeginUpload {
                    path: format!("{}/new", root.virtual_root()),
                    size: "3".into(),
                    expected: None,
                },
                Operation::ReviewEntry {
                    path,
                    purpose: rpc::ReviewPurpose::Delete,
                },
            ] {
                assert!(session
                    .execute(op, vec![], &CancellationToken::new())
                    .is_err());
            }
            assert_eq!(std::fs::read_dir(&base).unwrap().count(), 1);
            eprintln!("ordinary read remained available while full strict mount/ancestor qualification refused mutations");
        } else if !root.available {
            let native =
                festerm_unix_security::open_absolute_directory_read_nofollow(Path::new("/"))
                    .unwrap();
            assert_eq!(
                festerm_unix_security::open_contained_data_directory(
                    &native,
                    base.strip_prefix("/").unwrap()
                )
                .unwrap_err()
                .kind(),
                io::ErrorKind::Unsupported
            );
            assert!(!root.mutations_available);
        } else {
            assert!(
                root.mutations_available,
                "ownership-enforcing supported control"
            );
        }
        assert_eq!(std::fs::read(base.join("original")).unwrap(), b"original");
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn allowed_locations_actual_os_home_start_and_private_regular_file_read() {
        let home = festerm_unix_security::current_user_home().unwrap();
        let start = format!("/{}", home.strip_prefix("/").unwrap().to_str().unwrap());
        let mut session = locations_session(rpc::Policy {
            root: "/".into(),
            start_in: None,
            locations: Some(rpc::AllowedLocations {
                roots: vec!["/".into()],
                start: Some(rpc::LocationStart {
                    root: 0,
                    path: start.clone().try_into().unwrap(),
                }),
            }),
            write: false,
            delete: false,
        });
        let roots = session.filesystem.location_metadata().unwrap();
        assert_eq!(roots.len(), 1);
        if !roots[0].available {
            let root = festerm_unix_security::open_absolute_directory_read_nofollow(Path::new("/"))
                .unwrap();
            assert_eq!(
                festerm_unix_security::open_contained_data_directory(&root, Path::new(""))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::Unsupported
            );
            assert!(session
                .execute(
                    Operation::StartDirectory {},
                    Vec::new(),
                    &CancellationToken::new()
                )
                .is_err());
            assert!(session.filesystem.has_locations());
            eprintln!("HOME allowlist: unsupported native API floor; prepared Files unavailable, no HOME fixture or read");
            return;
        }
        assert!(
            matches!(run(&mut session,Operation::StartDirectory {}),Reply::StartDirectory {path} if path.as_str()==format!("{}{}",roots[0].virtual_root(),start))
        );
        let fixture = tempfile::Builder::new()
            .prefix(".festerm-allowlist-owned-")
            .tempdir_in(&home)
            .unwrap();
        let file = fixture.path().join("owned.txt");
        let payload = b"owned HOME namespace regular bytes";
        std::fs::write(&file, payload).unwrap();
        let path = format!(
            "{}/{}",
            roots[0].virtual_root(),
            file.strip_prefix("/").unwrap().to_str().unwrap()
        );
        assert!(
            matches!(run(&mut session,Operation::Stat {path:path.clone()}),Reply::Stat {metadata} if matches!(metadata.kind,rpc::Kind::File)&&metadata.size==payload.len().to_string())
        );
        let read = handle(run(
            &mut session,
            Operation::OpenRead { path: path.clone() },
        ));
        let (_, bytes) = session
            .execute(
                Operation::Read {
                    handle: read,
                    offset: "0".into(),
                    length: 64,
                },
                Vec::new(),
                &CancellationToken::new(),
            )
            .unwrap();
        assert_eq!(bytes.unwrap(), payload);
        run(&mut session, Operation::Close { handle: read });
        use std::os::unix::fs::PermissionsExt;
        struct Restore(std::path::PathBuf, std::fs::Permissions);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, self.1.clone());
            }
        }
        let restore = Restore(
            file.clone(),
            std::fs::metadata(&file).unwrap().permissions(),
        );
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o0)).unwrap();
        assert!(
            matches!(session.execute(Operation::OpenRead {path},Vec::new(),&CancellationToken::new()),Err(failure) if failure.code==ErrorCode::Denied),
            "trusted open denial must not retry ordinary NODEV admission"
        );
        drop(restore);
        assert_eq!(std::fs::read(file).unwrap(), payload);
    }
    #[test]
    #[cfg(unix)]
    fn allowed_locations_directory_page_rechecks_after_owned_parent_move() {
        let base = crate::test_support::private_directory("festerm-locations-page");
        let allowed = base.join("allowed");
        let inside = allowed.join("inside");
        let outside = base.join("outside");
        std::fs::create_dir(&allowed).unwrap();
        std::fs::create_dir(&inside).unwrap();
        std::fs::write(inside.join("visible"), b"original").unwrap();
        let mut session = locations_session(rpc::Policy {
            root: allowed.clone(),
            start_in: None,
            locations: Some(rpc::AllowedLocations {
                roots: vec![allowed.clone()],
                start: None,
            }),
            write: false,
            delete: false,
        });
        let roots = session.filesystem.location_metadata().unwrap();
        if !roots[0].available {
            #[cfg(target_os = "macos")]
            {
                let root =
                    festerm_unix_security::open_absolute_directory_read_nofollow(Path::new("/"))
                        .unwrap();
                assert_eq!(
                    festerm_unix_security::open_contained_data_directory(
                        &root,
                        allowed.strip_prefix("/").unwrap()
                    )
                    .unwrap_err()
                    .kind(),
                    io::ErrorKind::Unsupported
                );
                assert!(session
                    .execute(
                        Operation::OpenDirectory {
                            path: roots[0].virtual_root()
                        },
                        Vec::new(),
                        &CancellationToken::new()
                    )
                    .is_err());
                std::fs::remove_dir_all(base).unwrap();
                return;
            }
            #[cfg(not(target_os = "macos"))]
            panic!("local fixture unavailable");
        }
        let path = format!("{}/inside", roots[0].virtual_root());
        let id = handle(run(&mut session, Operation::OpenDirectory { path }));
        let Some(Handle::Directory { entries, .. }) = session.handles.get_mut(&id) else {
            panic!("directory handle")
        };
        let mut original = std::mem::replace(entries, Box::new(std::iter::empty()));
        let moved = outside.clone();
        let mut first = true;
        *entries = Box::new(std::iter::from_fn(move || {
            if first {
                first = false;
                std::fs::rename(&inside, &moved).unwrap();
            }
            original.next()
        }));
        assert!(
            session
                .execute(
                    Operation::ReadDirectory {
                        handle: id,
                        limit: rpc::PAGE_LIMIT
                    },
                    Vec::new(),
                    &CancellationToken::new()
                )
                .is_err(),
            "page collected after movement must be discarded"
        );
        assert_eq!(std::fs::read(outside.join("visible")).unwrap(), b"original");
        run(&mut session, Operation::Close { handle: id });
        std::fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn allowed_locations_cross_root_expected_handle_cannot_retarget_replacement() {
        let base = crate::test_support::private_directory("festerm-locations-review");
        let first = base.join("first");
        let second = base.join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        std::fs::write(first.join("same.txt"), b"first").unwrap();
        std::fs::write(second.join("same.txt"), b"second").unwrap();
        let mut session = locations_session(rpc::Policy {
            root: first.clone(),
            start_in: None,
            locations: Some(rpc::AllowedLocations {
                roots: vec![first.clone(), second.clone()],
                start: None,
            }),
            write: true,
            delete: true,
        });
        let roots = session.filesystem.location_metadata().unwrap();
        if roots.iter().all(|root| root.available) {
            let expected = handle(run(
                &mut session,
                Operation::OpenRead {
                    path: format!("{}/same.txt", roots[0].virtual_root()),
                },
            ));
            assert!(session
                .execute(
                    Operation::BeginUpload {
                        path: format!("{}/same.txt", roots[1].virtual_root()),
                        size: "3".into(),
                        expected: Some(expected)
                    },
                    Vec::new(),
                    &CancellationToken::new()
                )
                .is_err());
            run(&mut session, Operation::Close { handle: expected });
        } else {
            #[cfg(target_os = "macos")]
            {
                let root =
                    festerm_unix_security::open_absolute_directory_read_nofollow(Path::new("/"))
                        .unwrap();
                assert_eq!(
                    festerm_unix_security::open_contained_data_directory(
                        &root,
                        first.strip_prefix("/").unwrap()
                    )
                    .unwrap_err()
                    .kind(),
                    io::ErrorKind::Unsupported
                );
            }
            #[cfg(not(target_os = "macos"))]
            panic!("qualified local fixture unavailable");
        }
        assert_eq!(std::fs::read(first.join("same.txt")).unwrap(), b"first");
        assert_eq!(std::fs::read(second.join("same.txt")).unwrap(), b"second");
        assert_eq!(std::fs::read_dir(&first).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(&second).unwrap().count(), 1);
        drop(session);
        std::fs::remove_dir_all(base).unwrap();
    }
    #[test]
    #[cfg(unix)]
    fn allowed_locations_shared_outside_ancestor_never_becomes_mutation_authority() {
        use std::os::unix::fs::PermissionsExt;
        let base = crate::test_support::private_directory("festerm-locations-strict");
        let outside = base.join("outside");
        let allowed = outside.join("allowed");
        std::fs::create_dir(&outside).unwrap();
        std::fs::create_dir(&allowed).unwrap();
        std::fs::write(allowed.join("original"), b"preserve").unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o775)).unwrap();
        let mut session = locations_session(rpc::Policy {
            root: allowed.clone(),
            start_in: None,
            locations: Some(rpc::AllowedLocations {
                roots: vec![allowed.clone()],
                start: None,
            }),
            write: true,
            delete: true,
        });
        let roots = session.filesystem.location_metadata().unwrap();
        assert!(!roots[0].mutations_available);
        let virtual_root = roots[0].virtual_root();
        assert!(session
            .execute(
                Operation::Mkdir {
                    path: format!("{virtual_root}/new")
                },
                Vec::new(),
                &CancellationToken::new()
            )
            .is_err());
        assert!(session
            .execute(
                Operation::BeginUpload {
                    path: format!("{virtual_root}/new"),
                    size: "1".into(),
                    expected: None
                },
                Vec::new(),
                &CancellationToken::new()
            )
            .is_err());
        assert!(session
            .execute(
                Operation::OpenRead {
                    path: "/unknown-selector/original".into()
                },
                Vec::new(),
                &CancellationToken::new()
            )
            .is_err());
        assert!(!allowed.join("new").exists());
        assert_eq!(
            std::fs::read(allowed.join("original")).unwrap(),
            b"preserve"
        );
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn start_directory_is_nofollow_navigation_not_an_allowed_root() {
        let (path, mut session) = session(false);
        std::fs::create_dir(path.join("child")).unwrap();
        let mut policy = rpc::Policy {
            root: path.clone(),
            start_in: Some("/child".into()),
            locations: None,
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
            locations: None,
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
                deadline,
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
                deadline,
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
