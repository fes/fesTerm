//! Bounded read-only traversal within the explicit retained-root union.
use super::{same_object, Location};
use crate::{
    file_protocol as rpc,
    filesystem::{names, Filesystem},
};
use std::{
    collections::VecDeque,
    ffi::OsString,
    fs::File,
    io,
    path::{Component, Path, PathBuf},
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub(crate) const READ_DESCRIPTOR_LIMIT: usize = 48;
const NATIVE_SCRATCH: usize = 16;

pub(crate) struct ReadBudget {
    deadline: Instant,
    cancel: CancellationToken,
    work: usize,
    visits: usize,
    retained: usize,
    live: usize,
}
impl ReadBudget {
    pub(crate) fn new(deadline: Instant, cancel: CancellationToken) -> Self {
        Self {
            deadline,
            cancel,
            work: 8192,
            visits: 2048,
            retained: 0,
            live: 0,
        }
    }
    pub(crate) fn check(&mut self) -> io::Result<()> {
        if self.cancel.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if Instant::now() >= self.deadline || self.work == 0 {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.work -= 1;
        Ok(())
    }
    // Retained page/handle proofs and the resolver's current temporary handles
    // share one limit. Closed traversal handles do not consume later steps.
    pub(crate) fn check_live(&mut self, additional: usize) -> io::Result<()> {
        self.check()?;
        if self
            .retained
            .saturating_add(self.live)
            .saturating_add(additional)
            > READ_DESCRIPTOR_LIMIT
        {
            return Err(io::ErrorKind::OutOfMemory.into());
        }
        Ok(())
    }
    pub(crate) fn retain(&mut self, cost: usize) -> io::Result<()> {
        self.check_live(cost)?;
        self.retained += cost;
        Ok(())
    }
    fn native(&mut self, path: &Path) -> io::Result<()> {
        #[cfg(windows)]
        let cost = NATIVE_SCRATCH.saturating_add(path.components().count());
        #[cfg(not(windows))]
        let cost = {
            let _ = path;
            NATIVE_SCRATCH
        };
        self.check_live(cost)
    }
    fn visit(&mut self) -> io::Result<()> {
        self.check()?;
        if self.visits == 0 {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.visits -= 1;
        Ok(())
    }
    fn path(&mut self, path: &Path) -> io::Result<()> {
        self.check()?;
        for _ in path.components() {
            self.check()?;
        }
        Ok(())
    }
}
pub(crate) enum ReadError {
    Fatal(io::Error),
    Unavailable(io::Error),
}
impl ReadError {
    pub(crate) fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
    pub(crate) fn into_io(self) -> io::Error {
        match self {
            Self::Fatal(e) | Self::Unavailable(e) => e,
        }
    }
}
impl From<io::Error> for ReadError {
    fn from(error: io::Error) -> Self {
        Self::Fatal(error)
    }
}
fn unavailable(kind: io::ErrorKind) -> ReadError {
    ReadError::Unavailable(kind.into())
}
fn child(error: io::Error) -> ReadError {
    match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory | io::ErrorKind::Unsupported => {
            ReadError::Unavailable(error)
        }
        _ => ReadError::Fatal(error),
    }
}
fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
#[derive(Clone, PartialEq, Eq)]
struct RootKey {
    index: usize,
    selector: String,
}
impl RootKey {
    fn get<'a>(&self, fs: &'a Filesystem) -> io::Result<&'a Location> {
        fs.locations
            .as_ref()
            .and_then(|r| r.get(self.index))
            .filter(|r| r.selector == self.selector)
            .ok_or_else(refused)
    }
}
struct RootPin {
    key: RootKey,
    file: File,
}
struct DirectoryWitness {
    root: RootKey,
    relative: PathBuf,
    held: File,
}
#[cfg(unix)]
type NativeLink = festerm_unix_security::RetainedDataLink;
#[cfg(windows)]
type NativeLink = festerm_windows_security::RetainedDataLink;
struct LinkWitness {
    source: DirectoryWitness,
    pin: NativeLink,
    name: OsString,
    pending: VecDeque<OsString>,
    _parents: Vec<File>,
}
pub(crate) struct ReadProof {
    roots: Vec<RootPin>,
    links: Vec<LinkWitness>,
    popped: Vec<DirectoryWitness>,
    target: RootKey,
    relative: PathBuf,
    directory: bool,
    _parents: Vec<File>,
}
pub(crate) struct Resolved {
    pub(crate) file: File,
    pub(crate) read_only: bool,
    pub(crate) proof: ReadProof,
}
fn current(root: &Location, budget: &mut ReadBudget) -> io::Result<File> {
    budget.path(&root.path)?;
    budget.native(&root.path)?;
    Ok(root.current()?.try_clone()?.into_std_file())
}
fn directory(
    root: &Location,
    relative: &Path,
    budget: &mut ReadBudget,
) -> io::Result<(File, Vec<File>)> {
    let held = current(root, budget)?;
    budget.path(relative)?;
    contained_directory(&held, relative, budget)
}
fn contained_directory(
    parent: &File,
    relative: &Path,
    budget: &mut ReadBudget,
) -> io::Result<(File, Vec<File>)> {
    budget.native(relative)?;
    #[cfg(unix)]
    {
        Ok((
            festerm_unix_security::open_contained_data_directory(parent, relative)?,
            Vec::new(),
        ))
    }
    #[cfg(windows)]
    {
        festerm_windows_security::open_contained_data_directory(parent, relative)
    }
}
fn file(
    root: &Location,
    relative: &Path,
    budget: &mut ReadBudget,
) -> Result<(File, Vec<File>), ReadError> {
    let held = current(root, budget)?;
    budget.native(relative)?;
    #[cfg(target_os = "macos")]
    if let Some(proof) = festerm_unix_security::qualify_private_data_parent_checked(
        &root.path,
        &held,
        relative.parent().unwrap_or(Path::new("")),
        &mut || budget.check(),
    )? {
        return Ok((
            proof.open_file_checked(
                relative.file_name().ok_or_else(refused)?.as_ref(),
                &mut || budget.check(),
            )?,
            Vec::new(),
        ));
    }
    budget.path(relative)?;
    #[cfg(unix)]
    {
        festerm_unix_security::open_contained_data_file(&held, relative)
            .map(|file| (file, Vec::new()))
            .map_err(child)
    }
    #[cfg(windows)]
    {
        festerm_windows_security::open_contained_data_file(&held, relative).map_err(child)
    }
}
impl DirectoryWitness {
    fn validate(&self, fs: &Filesystem, budget: &mut ReadBudget) -> io::Result<()> {
        let (fresh, _) = directory(self.root.get(fs)?, &self.relative, budget)?;
        budget.check()?;
        if !same_object(&fresh, &self.held)? {
            return Err(refused());
        }
        Ok(())
    }
}
impl ReadProof {
    pub(crate) fn retained_cost(&self) -> usize {
        1 + self.roots.len()
            + self.popped.len()
            + self._parents.len()
            + self
                .links
                .iter()
                .map(|link| 5 + link._parents.len())
                .sum::<usize>()
    }
    pub(crate) fn validate(
        &self,
        fs: &Filesystem,
        held: &File,
        budget: &mut ReadBudget,
    ) -> io::Result<()> {
        // Standalone validation owns this proof too; a page or chunk may already
        // have charged it. A temporary floor avoids double charging that case.
        let retained = budget.retained;
        budget.retained = retained.max(self.retained_cost());
        let result = budget
            .check_live(0)
            .and_then(|()| self.validate_inner(fs, held, budget));
        budget.retained = retained;
        result
    }
    fn validate_inner(
        &self,
        fs: &Filesystem,
        held: &File,
        budget: &mut ReadBudget,
    ) -> io::Result<()> {
        for root in &self.roots {
            let fresh = current(root.key.get(fs)?, budget)?;
            budget.check()?;
            if !same_object(&fresh, &root.file)? {
                return Err(refused());
            }
        }
        for witness in &self.popped {
            witness.validate(fs, budget)?;
        }
        for witness in &self.links {
            witness.source.validate(fs, budget)?;
            budget.native(Path::new(""))?;
            #[cfg(unix)]
            witness.pin.revalidate_checked(&mut || budget.check())?;
            #[cfg(windows)]
            {
                budget.check()?;
                witness.pin.revalidate()?;
            }
            witness.source.validate(fs, budget)?;
        }
        let root = self.target.get(fs)?;
        let (fresh, _) = if self.directory {
            directory(root, &self.relative, budget)?
        } else {
            file(root, &self.relative, budget).map_err(ReadError::into_io)?
        };
        budget.check()?;
        if !same_object(&fresh, held)? {
            return Err(refused());
        }
        Ok(())
    }
}
struct Cursor {
    root: RootKey,
    relative: PathBuf,
    directory: File,
    parents: Vec<File>,
}
fn enter(
    fs: &Filesystem,
    index: usize,
    anchors: &mut Vec<RootPin>,
    budget: &mut ReadBudget,
) -> io::Result<Cursor> {
    let root = fs
        .locations
        .as_ref()
        .and_then(|r| r.get(index))
        .ok_or_else(refused)?;
    let held = current(root, budget)?;
    budget.check_live(1)?;
    budget.live += 1;
    let key = RootKey {
        index,
        selector: root.selector.clone(),
    };
    if let Some(pin) = anchors.iter().find(|pin| pin.key == key) {
        if !same_object(&pin.file, &held)? {
            return Err(refused());
        }
    } else {
        if anchors.len() == 8 {
            return Err(refused());
        }
        budget.check_live(1)?;
        budget.live += 1;
        anchors.push(RootPin {
            key: key.clone(),
            file: held.try_clone()?,
        });
    }
    Ok(Cursor {
        root: key,
        relative: PathBuf::new(),
        directory: held,
        parents: Vec::new(),
    })
}
// Inspect the native payload before Path's component/prefix normalization.
// Reserved publication/capture names are never ordinary readable data, even
// through an alias or with a different ASCII case.
fn raw_target(path: &Path) -> Result<(), ReadError> {
    let bytes = path.as_os_str().as_encoded_bytes();
    #[cfg(unix)]
    if bytes.len() > 1 && (bytes.ends_with(b"/") || bytes.ends_with(b"/.")) {
        return Err(unavailable(io::ErrorKind::Unsupported));
    }
    for part in bytes.split(|b| *b == b'/' || cfg!(windows) && *b == b'\\') {
        if part.len() >= b".festerm-transfer-".len()
            && part[..b".festerm-transfer-".len()].eq_ignore_ascii_case(b".festerm-transfer-")
        {
            return Err(unavailable(io::ErrorKind::PermissionDenied));
        }
    }
    Ok(())
}
fn remember(popped: &mut Vec<DirectoryWitness>, witness: DirectoryWitness) -> io::Result<()> {
    if let Some(previous) = popped
        .iter()
        .find(|p| p.root == witness.root && p.relative == witness.relative)
    {
        if !same_object(&previous.held, &witness.held)? {
            return Err(refused());
        }
    } else {
        popped.push(witness);
    }
    Ok(())
}
fn components(path: &Path) -> Result<VecDeque<OsString>, ReadError> {
    let mut parts = VecDeque::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => parts.push_back(name.to_owned()),
            Component::CurDir => parts.push_back(OsString::from(".")),
            Component::ParentDir => parts.push_back(OsString::from("..")),
            _ => return Err(unavailable(io::ErrorKind::Unsupported)),
        }
    }
    Ok(parts)
}
fn active_bound(relative: &Path, pending: &VecDeque<OsString>) -> Result<(), ReadError> {
    let size = relative.as_os_str().as_encoded_bytes().len()
        + pending
            .iter()
            .map(|x| x.as_encoded_bytes().len() + 1)
            .sum::<usize>();
    if size > 4096 {
        Err(unavailable(io::ErrorKind::Unsupported))
    } else {
        Ok(())
    }
}
fn absolute(
    fs: &Filesystem,
    target: &Path,
    anchors: &mut Vec<RootPin>,
    budget: &mut ReadBudget,
) -> Result<(Cursor, VecDeque<OsString>), ReadError> {
    let roots = fs
        .locations
        .as_ref()
        .ok_or_else(|| unavailable(io::ErrorKind::Unsupported))?;
    let (index, relative) = roots
        .iter()
        .enumerate()
        .filter_map(|(i, root)| target.strip_prefix(&root.path).ok().map(|rel| (i, rel)))
        .max_by_key(|(i, _)| roots[*i].path.components().count())
        .ok_or_else(|| unavailable(io::ErrorKind::PermissionDenied))?;
    Ok((enter(fs, index, anchors, budget)?, components(relative)?))
}
fn upward(
    fs: &Filesystem,
    cursor: &mut Cursor,
    anchors: &mut Vec<RootPin>,
    popped: &mut Vec<DirectoryWitness>,
    budget: &mut ReadBudget,
) -> Result<(), ReadError> {
    // A component consumed before '..' remains a disclosure dependency.
    budget.check_live(1)?;
    budget.live += 1;
    let witness = DirectoryWitness {
        root: cursor.root.clone(),
        relative: cursor.relative.clone(),
        held: cursor.directory.try_clone()?,
    };
    witness.validate(fs, budget)?;
    if !cursor.relative.as_os_str().is_empty() {
        let child_name = cursor.relative.file_name().ok_or_else(refused)?.to_owned();
        let mut parent_relative = cursor.relative.clone();
        parent_relative.pop();
        #[cfg(unix)]
        let parent = {
            let (parent, _) = directory(cursor.root.get(fs)?, &parent_relative, budget)?;
            // A fresh parent lookup alone is insufficient: verify its actual
            // no-follow child is the directory whose '..' is being consumed.
            let (child, _) = contained_directory(&parent, Path::new(&child_name), budget)?;
            if !same_object(&child, &cursor.directory)? {
                return Err(ReadError::Fatal(refused()));
            }
            parent
        };
        #[cfg(windows)]
        let parent = {
            let _ = child_name;
            cursor.parents.pop().ok_or_else(refused)?
        };
        remember(popped, witness)?;
        cursor.relative = parent_relative;
        cursor.directory = parent;
        return Ok(());
    }
    let root = cursor.root.get(fs)?;
    let Some(parent) = root.path.parent() else {
        // Native volume-root '..' stays at the same admitted root.
        if root
            .path
            .components()
            .all(|part| !matches!(part, Component::Normal(_)))
        {
            return Ok(());
        }
        return Err(unavailable(io::ErrorKind::PermissionDenied));
    };
    let (mut enclosing, relative) = absolute(fs, parent, anchors, budget)?;
    // Verify the actual nested-root relationship, not just the lexical prefix.
    let outer = enclosing.root.get(fs)?;
    let child_relative = root
        .path
        .strip_prefix(&outer.path)
        .map_err(|_| unavailable(io::ErrorKind::PermissionDenied))?;
    let (same, _) = directory(outer, child_relative, budget)?;
    if !same_object(&same, &cursor.directory)? {
        return Err(ReadError::Fatal(refused()));
    }
    drop(same);
    let mut parent_relative = PathBuf::new();
    for part in relative {
        budget.visit()?;
        parent_relative.push(part);
    }
    // One qualified relookup avoids retaining an uncharged intermediate stack;
    // Windows reserves its complete returned vector before native allocation.
    let (directory, parents) = directory(outer, &parent_relative, budget)?;
    enclosing.directory = directory;
    enclosing.parents = parents;
    enclosing.relative = parent_relative;
    *cursor = enclosing;
    Ok(())
}
fn pin(
    root: &Location,
    parent_relative: &Path,
    parent: &File,
    name: &Path,
    budget: &mut ReadBudget,
) -> Result<NativeLink, ReadError> {
    budget.native(parent_relative)?;
    #[cfg(target_os = "macos")]
    {
        let held = current(root, budget)?;
        if let Some(proof) = festerm_unix_security::qualify_private_data_parent_checked(
            &root.path,
            &held,
            parent_relative,
            &mut || budget.check(),
        )? {
            return proof
                .pin_link_checked(parent, name, &mut || budget.check())
                .map_err(ReadError::Fatal);
        }
    }
    budget.check()?;
    #[cfg(not(target_os = "macos"))]
    let _ = (root, parent_relative);
    #[cfg(unix)]
    {
        festerm_unix_security::pin_data_link(parent, name).map_err(child)
    }
    #[cfg(windows)]
    {
        festerm_windows_security::RetainedDataLink::open(parent, name).map_err(child)
    }
}
fn target(pin: &NativeLink) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(pin.target().to_vec()).into()
    }
    #[cfg(windows)]
    {
        match pin.target() {
            festerm_windows_security::WindowsLinkTarget::ParentRelative(p)
            | festerm_windows_security::WindowsLinkTarget::AbsoluteDos(p) => p.clone(),
        }
    }
}

pub(crate) fn resolve_read(
    fs: &Filesystem,
    path: &str,
    want: Option<rpc::Kind>,
    budget: &mut ReadBudget,
) -> Result<Resolved, ReadError> {
    resolve(fs, path, want, None, budget)
}
pub(crate) fn resolve_read_child(
    fs: &Filesystem,
    path: &str,
    held_parent: &File,
    budget: &mut ReadBudget,
) -> Result<Resolved, ReadError> {
    resolve(fs, path, None, Some(held_parent), budget)
}
fn resolve(
    fs: &Filesystem,
    path: &str,
    want: Option<rpc::Kind>,
    expected: Option<&File>,
    budget: &mut ReadBudget,
) -> Result<Resolved, ReadError> {
    let live = budget.live;
    let retained = budget.retained;
    let result = resolve_inner(fs, path, want, expected, budget);
    budget.live = live;
    if result.is_err() {
        budget.retained = retained;
    }
    result
}
fn resolve_inner(
    fs: &Filesystem,
    path: &str,
    want: Option<rpc::Kind>,
    expected: Option<&File>,
    budget: &mut ReadBudget,
) -> Result<Resolved, ReadError> {
    budget.check()?;
    let roots = fs
        .locations
        .as_ref()
        .ok_or_else(|| unavailable(io::ErrorKind::Unsupported))?;
    let mut input = names(path)
        .map_err(|_| unavailable(io::ErrorKind::InvalidInput))?
        .into_iter();
    let selector = input
        .next()
        .ok_or_else(|| unavailable(io::ErrorKind::InvalidInput))?;
    let index = roots
        .iter()
        .position(|r| r.selector == selector)
        .ok_or_else(|| unavailable(io::ErrorKind::InvalidInput))?;
    let mut anchors = Vec::new();
    let mut cursor = enter(fs, index, &mut anchors, budget)?;
    let mut pending: VecDeque<_> = input.map(OsString::from).collect();
    let mut witnesses = Vec::new();
    let mut popped = Vec::new();
    let mut text_bytes = 0;
    let mut expected_pending = expected.is_some();
    let mut checked_parent: Option<(RootKey, PathBuf)> = None;
    if expected_pending && pending.is_empty() {
        return Err(ReadError::Fatal(refused()));
    }
    let outcome = (|| -> Result<_, ReadError> {
        loop {
            budget.live = anchors.len()
                + popped.len()
                + 1
                + cursor.parents.len()
                + witnesses
                    .iter()
                    .map(|link: &LinkWitness| 5 + link._parents.len())
                    .sum::<usize>();
            budget.check_live(0)?;
            active_bound(&cursor.relative, &pending)?;
            let Some(name) = pending.pop_front() else {
                break Ok((
                    cursor.directory,
                    true,
                    cursor.parents,
                    cursor.root,
                    cursor.relative,
                ));
            };
            budget.visit()?;
            if name == "." {
                continue;
            }
            if name == ".." {
                upward(fs, &mut cursor, &mut anchors, &mut popped, budget)?;
                continue;
            }
            let bind_child = expected_pending && pending.is_empty();
            if bind_child {
                expected_pending = false;
                check_parent(
                    fs,
                    &cursor.root,
                    &cursor.relative,
                    &cursor.directory,
                    expected.ok_or_else(refused)?,
                    budget,
                )?;
                checked_parent = Some((cursor.root.clone(), cursor.relative.clone()));
            }
            budget.check_live(1)?;
            let info = {
                let parent = cap_std::fs::Dir::from_std_file(cursor.directory.try_clone()?);
                budget.check()?;
                parent.symlink_metadata(&name).map_err(child)?
            };
            if bind_child && !info.file_type().is_symlink() {
                return Err(ReadError::Fatal(refused()));
            }
            #[cfg(windows)]
            {
                use cap_std::fs::MetadataExt;
                if unsupported_reparse(info.file_type().is_symlink(), info.file_attributes()) {
                    return Err(unavailable(io::ErrorKind::Unsupported));
                }
            }
            if info.file_type().is_symlink() {
                // The suffix is part of the state: alias -> '.' can occur more
                // than once in a finite request such as alias/alias/file.
                if let Some(previous) = witnesses.iter().find(|previous| {
                    previous.source.root == cursor.root
                        && previous.source.relative == cursor.relative
                        && previous.name == name
                        && previous.pending == pending
                }) {
                    if !same_object(&previous.source.held, &cursor.directory)? {
                        return Err(ReadError::Fatal(refused()));
                    }
                    previous.source.validate(fs, budget)?;
                    budget.native(Path::new(""))?;
                    #[cfg(unix)]
                    previous.pin.revalidate_checked(&mut || budget.check())?;
                    #[cfg(windows)]
                    previous.pin.revalidate()?;
                    previous.source.validate(fs, budget)?;
                    return Err(unavailable(io::ErrorKind::Unsupported));
                }
                if witnesses.len() == 8 {
                    return Err(unavailable(io::ErrorKind::Unsupported));
                }
                budget.check_live(1)?;
                budget.live += 1;
                let source = DirectoryWitness {
                    root: cursor.root.clone(),
                    relative: cursor.relative.clone(),
                    held: cursor.directory.try_clone()?,
                };
                source.validate(fs, budget)?;
                let link = pin(
                    cursor.root.get(fs)?,
                    &cursor.relative,
                    &cursor.directory,
                    Path::new(&name),
                    budget,
                )?;
                budget.live += 4;
                budget.check_live(0)?;
                let path = target(&link);
                raw_target(&path)?;
                text_bytes += path.as_os_str().as_encoded_bytes().len();
                if path.as_os_str().as_encoded_bytes().len() > 4096 || text_bytes > 32768 {
                    return Err(unavailable(io::ErrorKind::Unsupported));
                }
                let (fresh, parents) = directory(cursor.root.get(fs)?, &cursor.relative, budget)?;
                if !same_object(&fresh, &source.held)? {
                    return Err(ReadError::Fatal(refused()));
                }
                drop(fresh);
                budget.live += parents.len();
                budget.check_live(0)?;
                source.validate(fs, budget)?;
                witnesses.push(LinkWitness {
                    source,
                    pin: link,
                    name: name.clone(),
                    pending: pending.clone(),
                    _parents: parents,
                });
                let mut expanded = if path.is_absolute() {
                    let (next, expanded) = absolute(fs, &path, &mut anchors, budget)?;
                    cursor = next;
                    expanded
                } else {
                    components(&path)?
                };
                expanded.append(&mut pending);
                pending = expanded;
                continue;
            }
            let next = cursor.relative.join(&name);
            if info.is_dir() {
                budget.check()?;
                let (directory, _duplicate_parents) =
                    contained_directory(&cursor.directory, Path::new(&name), budget)
                        .map_err(child)?;
                #[cfg(windows)]
                cursor.parents.push(cursor.directory);
                drop(_duplicate_parents);
                cursor.directory = directory;
                cursor.relative = next;
            } else if info.is_file() && pending.is_empty() {
                if matches!(want, Some(rpc::Kind::Directory)) {
                    return Err(unavailable(io::ErrorKind::NotADirectory));
                }
                let (file, parents) = file(cursor.root.get(fs)?, &next, budget)?;
                break Ok((file, false, parents, cursor.root, next));
            } else {
                return Err(unavailable(io::ErrorKind::Unsupported));
            }
        }
    })();
    let (held, directory_target, parents, target, relative) = match outcome {
        Ok(value) => value,
        Err(error) if error.is_unavailable() => {
            if expected_pending {
                return Err(ReadError::Fatal(refused()));
            }
            validate_partial(fs, &anchors, &witnesses, &popped, budget)?;
            if let Some((root, relative)) = &checked_parent {
                let (fresh, _) = directory(root.get(fs)?, relative, budget)?;
                if !same_object(&fresh, expected.ok_or_else(refused)?)? {
                    return Err(ReadError::Fatal(refused()));
                }
            }
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    budget.live = 1
        + parents.len()
        + anchors.len()
        + popped.len()
        + witnesses
            .iter()
            .map(|link| 5 + link._parents.len())
            .sum::<usize>();
    budget.check_live(0)?;
    if matches!(want, Some(rpc::Kind::Directory)) && !directory_target
        || matches!(want, Some(rpc::Kind::File)) && directory_target
        || matches!(want, Some(rpc::Kind::Unsupported))
    {
        validate_partial(fs, &anchors, &witnesses, &popped, budget)?;
        return Err(unavailable(io::ErrorKind::InvalidInput));
    }
    let proof = ReadProof {
        roots: anchors,
        links: witnesses,
        popped,
        target,
        relative,
        directory: directory_target,
        _parents: parents,
    };
    budget.live = 0;
    budget.retain(proof.retained_cost())?;
    proof.validate(fs, &held, budget)?;
    Ok(Resolved {
        file: held,
        read_only: !proof.links.is_empty(),
        proof,
    })
}
#[cfg(any(windows, test))]
fn unsupported_reparse(is_link: bool, attributes: u32) -> bool {
    !is_link && attributes & 0x400 != 0
}
fn check_parent(
    fs: &Filesystem,
    root: &RootKey,
    relative: &Path,
    cursor: &File,
    expected: &File,
    budget: &mut ReadBudget,
) -> io::Result<()> {
    if !same_object(cursor, expected)? {
        return Err(refused());
    }
    let (fresh, _) = directory(root.get(fs)?, relative, budget)?;
    if !same_object(&fresh, expected)? {
        return Err(refused());
    }
    Ok(())
}
fn validate_partial(
    fs: &Filesystem,
    roots: &[RootPin],
    links: &[LinkWitness],
    popped: &[DirectoryWitness],
    budget: &mut ReadBudget,
) -> io::Result<()> {
    for pin in roots {
        let fresh = current(pin.key.get(fs)?, budget)?;
        budget.check()?;
        if !same_object(&fresh, &pin.file)? {
            return Err(refused());
        }
    }
    for witness in popped {
        witness.validate(fs, budget)?;
    }
    for witness in links {
        witness.source.validate(fs, budget)?;
        budget.native(Path::new(""))?;
        #[cfg(unix)]
        witness.pin.revalidate_checked(&mut || budget.check())?;
        #[cfg(windows)]
        {
            budget.check()?;
            witness.pin.revalidate()?;
        }
        witness.source.validate(fs, budget)?;
    }
    Ok(())
}

impl std::fmt::Debug for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fatal(e) => f.debug_tuple("Fatal").field(&e.kind()).finish(),
            Self::Unavailable(e) => f.debug_tuple("Unavailable").field(&e.kind()).finish(),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, time::Duration};
    fn budget() -> ReadBudget {
        ReadBudget::new(
            Instant::now() + Duration::from_secs(30),
            CancellationToken::new(),
        )
    }
    fn fixture() -> Option<(tempfile::TempDir, std::sync::Arc<Filesystem>)> {
        #[cfg(target_os = "macos")]
        let base = tempfile::Builder::new()
            .prefix(".festerm-readable-links-")
            .tempdir_in(festerm_unix_security::current_user_home().unwrap())
            .unwrap();
        #[cfg(not(target_os = "macos"))]
        let base = tempfile::tempdir().unwrap();
        for dir in ["a", "b", "outside"] {
            std::fs::create_dir(base.path().join(dir)).unwrap();
        }
        std::fs::create_dir(base.path().join("b/inner")).unwrap();
        std::fs::write(base.path().join("b/inner/data"), b"retained target bytes").unwrap();
        std::fs::write(base.path().join("outside/canary"), b"outside original").unwrap();
        let fs = make_fs(&[base.path().join("a"), base.path().join("b")]);
        if fs
            .locations
            .as_ref()
            .unwrap()
            .iter()
            .any(|r| r.current().is_err())
        {
            #[cfg(target_os = "macos")]
            {
                let parent = File::open(base.path()).unwrap();
                assert_eq!(
                    festerm_unix_security::open_contained_data_directory(&parent, Path::new(""))
                        .unwrap_err()
                        .kind(),
                    io::ErrorKind::Unsupported
                );
                let path = fs.locations.as_ref().unwrap()[0].virtual_root();
                assert!(!resolve_read(&fs, &path, None, &mut budget())
                    .err()
                    .unwrap()
                    .is_unavailable());
                eprintln!(
                    "readable_links: unavailable native macOS floor, no positive resolver claim"
                );
                return None;
            }
            #[cfg(not(target_os = "macos"))]
            panic!("native supported filesystem prerequisite unavailable");
        }
        #[cfg(windows)]
        {
            let probe = base.path().join("a/privilege-control");
            match std::os::windows::fs::symlink_file(base.path().join("b/inner/data"), &probe) {
                Ok(()) => std::fs::remove_file(probe).unwrap(),
                Err(error)
                    if error.raw_os_error() == Some(1314)
                        && std::env::var_os("FESTERM_REQUIRE_WINDOWS_SYMLINKS")
                            .is_none_or(|value| value.is_empty()) =>
                {
                    eprintln!(
                        "Native Windows symlink qualification deferred; creation returned 1314"
                    );
                    return None;
                }
                Err(error) => panic!("native Windows link fixture prerequisite: {error:?}"),
            }
        }
        Some((base, fs))
    }
    fn make_fs(roots: &[PathBuf]) -> std::sync::Arc<Filesystem> {
        Filesystem::open(&rpc::Policy {
            root: roots[0].clone(),
            start_in: None,
            locations: Some(rpc::AllowedLocations {
                roots: roots.to_vec(),
                start: None,
            }),
            write: true,
            delete: true,
        })
        .unwrap()
    }
    fn virtual_path(fs: &Filesystem, index: usize, suffix: &str) -> String {
        format!(
            "{}/{}",
            fs.locations.as_ref().unwrap()[index].virtual_root(),
            suffix
        )
    }
    fn link_dir(target: &Path, path: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, path).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(target, path).unwrap();
    }
    fn link_file(target: &Path, path: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, path).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(target, path).unwrap();
    }
    #[test]
    fn readable_links_union_directory_file_and_direct_access_keep_provenance() {
        let Some((base, fs)) = fixture() else {
            return;
        };
        link_dir(&base.path().join("b/inner"), &base.path().join("a/folder"));
        link_file(
            &base.path().join("b/inner/data"),
            &base.path().join("a/file"),
        );
        let dir = resolve_read(
            &fs,
            &virtual_path(&fs, 0, "folder"),
            Some(rpc::Kind::Directory),
            &mut budget(),
        )
        .unwrap();
        assert!(dir.read_only && dir.file.metadata().unwrap().is_dir());
        dir.proof.validate(&fs, &dir.file, &mut budget()).unwrap();
        for suffix in ["file", "folder/data"] {
            let mut result = resolve_read(
                &fs,
                &virtual_path(&fs, 0, suffix),
                Some(rpc::Kind::File),
                &mut budget(),
            )
            .unwrap();
            assert!(result.read_only);
            let mut bytes = Vec::new();
            result.file.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"retained target bytes");
            result
                .proof
                .validate(&fs, &result.file, &mut budget())
                .unwrap();
        }
        let direct = resolve_read(
            &fs,
            &virtual_path(&fs, 1, "inner/data"),
            None,
            &mut budget(),
        )
        .unwrap();
        assert!(!direct.read_only);
        assert!(
            fs.read_file(&virtual_path(&fs, 0, "folder/data")).is_err(),
            "strict route must remain no-follow"
        );
        assert_eq!(
            std::fs::read(base.path().join("outside/canary")).unwrap(),
            b"outside original"
        );
    }
    #[test]
    fn readable_links_outside_intermediate_dangling_and_loop_are_unavailable() {
        let Some((base, fs)) = fixture() else {
            return;
        };
        link_file(
            &base.path().join("outside/canary"),
            &base.path().join("a/outside"),
        );
        link_file(Path::new("missing"), &base.path().join("a/dangling"));
        link_file(Path::new("loop"), &base.path().join("a/loop"));
        link_file(
            Path::new("../b/inner/data"),
            &base.path().join("a/intermediate"),
        );
        for suffix in ["outside", "dangling", "loop", "intermediate"] {
            assert!(
                resolve_read(&fs, &virtual_path(&fs, 0, suffix), None, &mut budget())
                    .err()
                    .unwrap()
                    .is_unavailable(),
                "{suffix}"
            );
        }
        assert_eq!(
            std::fs::read(base.path().join("outside/canary")).unwrap(),
            b"outside original"
        );
    }
    #[test]
    fn readable_links_nested_root_upward_uses_verified_enclosing_root() {
        let Some((base, _fs)) = fixture() else {
            return;
        };
        let fs = make_fs(&[base.path().to_owned(), base.path().join("a")]);
        link_file(
            Path::new("../b/inner/data"),
            &base.path().join("a/through-parent"),
        );
        let value = resolve_read(
            &fs,
            &virtual_path(&fs, 1, "through-parent"),
            Some(rpc::Kind::File),
            &mut budget(),
        )
        .unwrap();
        assert!(value.read_only);
        value
            .proof
            .validate(&fs, &value.file, &mut budget())
            .unwrap();
    }
    #[test]
    fn readable_links_budget_cancel_and_unsupported_resource_errors_are_fatal() {
        let Some((_base, fs)) = fixture() else {
            return;
        };
        let path = virtual_path(&fs, 1, "inner/data");
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut canceled = ReadBudget::new(Instant::now() + Duration::from_secs(30), cancel);
        assert!(!resolve_read(&fs, &path, None, &mut canceled)
            .err()
            .unwrap()
            .is_unavailable());
        let mut expired = ReadBudget::new(Instant::now(), CancellationToken::new());
        assert!(!resolve_read(&fs, &path, None, &mut expired)
            .err()
            .unwrap()
            .is_unavailable());
        let mut exhausted = budget();
        exhausted.work = 0;
        assert!(!resolve_read(&fs, &path, None, &mut exhausted)
            .err()
            .unwrap()
            .is_unavailable());
        let mut shared = budget();
        resolve_read(&fs, &path, None, &mut shared).unwrap();
        assert!(shared.work < 8192);
        shared.visits = 0;
        assert!(!resolve_read(&fs, &path, None, &mut shared)
            .err()
            .unwrap()
            .is_unavailable());
        assert!(!child(io::ErrorKind::OutOfMemory.into()).is_unavailable());
        let mut pending = VecDeque::new();
        pending.push_back(OsString::from("x".repeat(4097)));
        assert!(active_bound(Path::new(""), &pending).is_err());
    }
    #[test]
    #[cfg(unix)]
    fn readable_links_alias_root_and_popped_directory_changes_fail_validation() {
        let Some((base, fs)) = fixture() else {
            return;
        };
        std::fs::create_dir(base.path().join("b/ordinary")).unwrap();
        link_file(
            &base.path().join("b/ordinary/../inner/data"),
            &base.path().join("a/alias"),
        );
        let resolved =
            resolve_read(&fs, &virtual_path(&fs, 0, "alias"), None, &mut budget()).unwrap();
        std::fs::rename(base.path().join("b/ordinary"), base.path().join("b/moved")).unwrap();
        link_dir(
            &base.path().join("outside"),
            &base.path().join("b/ordinary"),
        );
        assert!(resolved
            .proof
            .validate(&fs, &resolved.file, &mut budget())
            .is_err());
        std::fs::remove_file(base.path().join("b/ordinary")).unwrap();
        std::fs::rename(base.path().join("b/moved"), base.path().join("b/ordinary")).unwrap();
        resolved
            .proof
            .validate(&fs, &resolved.file, &mut budget())
            .unwrap();
        std::fs::remove_file(base.path().join("a/alias")).unwrap();
        link_file(
            &base.path().join("b/inner/data"),
            &base.path().join("a/alias"),
        );
        assert!(resolved
            .proof
            .validate(&fs, &resolved.file, &mut budget())
            .is_err());
        let current =
            resolve_read(&fs, &virtual_path(&fs, 0, "alias"), None, &mut budget()).unwrap();
        std::fs::rename(base.path().join("b"), base.path().join("old-b")).unwrap();
        std::fs::create_dir(base.path().join("b")).unwrap();
        assert!(current
            .proof
            .validate(&fs, &current.file, &mut budget())
            .is_err());
        let error = resolve_read(&fs, &virtual_path(&fs, 0, "alias"), None, &mut budget())
            .err()
            .unwrap();
        assert!(!error.is_unavailable());
    }
    #[test]
    fn readable_links_raw_reserved_targets_and_unknown_reparse_are_not_admitted() {
        for path in [
            ".festerm-transfer-owned",
            "ok/.FeStErM-TrAnSfEr-owned/data",
            "/allowed/.FESTERM-TRANSFER-owned",
        ] {
            assert!(raw_target(Path::new(path)).is_err(), "{path}");
        }
        assert!(raw_target(Path::new("ordinary/../data")).is_ok());
        assert!(unsupported_reparse(false, 0x400));
        assert!(!unsupported_reparse(true, 0x400));
        assert!(!unsupported_reparse(false, 0x10));
        let mut limited = budget();
        limited.retain(READ_DESCRIPTOR_LIMIT).unwrap();
        assert!(limited.check_live(1).is_err());
        assert!(limited.retain(1).is_err());
        assert_eq!(limited.retained, READ_DESCRIPTOR_LIMIT);
    }
    #[test]
    #[cfg(unix)]
    fn readable_links_native_reserved_and_trailing_syntax_refuse_before_normalization() {
        let Some((base, fs)) = fixture() else {
            return;
        };
        let private = base.path().join("b/inner/.FeStErM-TrAnSfEr-owned");
        std::fs::write(&private, b"reserved canary").unwrap();
        for (leaf, target) in [
            ("relative-reserved", ".FeStErM-TrAnSfEr-owned"),
            ("relative-file-slash", "data/"),
            ("relative-file-dot", "data/."),
        ] {
            link_file(Path::new(target), &base.path().join("b/inner").join(leaf));
            assert!(
                resolve_read(
                    &fs,
                    &virtual_path(&fs, 1, &format!("inner/{leaf}")),
                    None,
                    &mut budget()
                )
                .err()
                .unwrap()
                .is_unavailable(),
                "{leaf}"
            );
        }
        let targets = [
            PathBuf::from("../b/inner/.FeStErM-TrAnSfEr-owned"),
            private.clone(),
            PathBuf::from("../b/inner/data/"),
            PathBuf::from("../b/inner/data/."),
            base.path().join("b/inner/data/"),
            base.path().join("b/inner/data/."),
            base.path().join("b/inner/"),
        ];
        for (i, target) in targets.iter().enumerate() {
            let leaf = format!("raw-{i}");
            link_file(target, &base.path().join("a").join(&leaf));
            assert!(
                resolve_read(&fs, &virtual_path(&fs, 0, &leaf), None, &mut budget())
                    .err()
                    .unwrap()
                    .is_unavailable(),
                "{i}"
            );
        }
        link_dir(
            &base.path().join("b/inner"),
            &base.path().join("a/directory-control"),
        );
        assert!(resolve_read(
            &fs,
            &virtual_path(&fs, 0, "directory-control"),
            Some(rpc::Kind::Directory),
            &mut budget()
        )
        .is_ok());
        assert_eq!(std::fs::read(private).unwrap(), b"reserved canary");
    }
    #[test]
    fn readable_links_listed_child_binds_original_parent_and_link_kind() {
        let Some((base, fs)) = fixture() else {
            return;
        };
        std::fs::create_dir(base.path().join("b/second")).unwrap();
        for dir in ["b/inner", "b/second"] {
            link_file(
                &base.path().join("b/inner/data"),
                &base.path().join(dir).join("leaf"),
            );
        }
        link_dir(&base.path().join("b/inner"), &base.path().join("a/page"));
        let page = resolve_read(
            &fs,
            &virtual_path(&fs, 0, "page"),
            Some(rpc::Kind::Directory),
            &mut budget(),
        )
        .unwrap();
        let held_parent = page.file.try_clone().unwrap();
        drop(page); // Windows pins intentionally forbid alias replacement while held.
        let path = virtual_path(&fs, 0, "page/leaf");
        let valid = resolve_read_child(&fs, &path, &held_parent, &mut budget()).unwrap();
        assert!(valid.read_only);
        drop(valid);
        #[cfg(unix)]
        std::fs::remove_file(base.path().join("a/page")).unwrap();
        #[cfg(windows)]
        std::fs::remove_dir(base.path().join("a/page")).unwrap();
        link_dir(&base.path().join("b/second"), &base.path().join("a/page"));
        assert!(!resolve_read_child(&fs, &path, &held_parent, &mut budget())
            .err()
            .unwrap()
            .is_unavailable());
        // Even with an alias ancestor, a former link becoming an ordinary leaf
        // is a fatal page-binding mismatch, not an inherited read-only success.
        let direct = virtual_path(&fs, 1, "inner/leaf");
        std::fs::remove_file(base.path().join("b/inner/leaf")).unwrap();
        std::fs::write(base.path().join("b/inner/leaf"), b"replacement").unwrap();
        assert!(
            !resolve_read_child(&fs, &direct, &held_parent, &mut budget())
                .err()
                .unwrap()
                .is_unavailable()
        );
        std::fs::remove_file(base.path().join("b/inner/leaf")).unwrap();
        assert!(
            resolve_read_child(&fs, &direct, &held_parent, &mut budget())
                .err()
                .unwrap()
                .is_unavailable()
        );
        link_file(Path::new("missing"), &base.path().join("b/inner/leaf"));
        assert!(
            resolve_read_child(&fs, &direct, &held_parent, &mut budget())
                .err()
                .unwrap()
                .is_unavailable()
        );
    }
    #[test]
    #[cfg(unix)]
    fn readable_links_unix_depth_is_constant_and_popped_witnesses_coalesce() {
        let Some((base, fs)) = fixture() else {
            return;
        };
        let mut path = base.path().join("b");
        let mut suffix = String::new();
        for _ in 0..70 {
            path.push("d");
            std::fs::create_dir(&path).unwrap();
            suffix.push_str("d/");
        }
        suffix.pop();
        let mut shared = budget();
        let result = resolve_read(
            &fs,
            &virtual_path(&fs, 1, &suffix),
            Some(rpc::Kind::Directory),
            &mut shared,
        )
        .unwrap();
        assert_eq!(result.proof.retained_cost(), 2);
        assert_eq!(shared.retained, 2);
        result
            .proof
            .validate(&fs, &result.file, &mut budget())
            .unwrap();
        drop(result);
        link_file(
            &base.path().join("b/inner/../inner/../inner/data"),
            &base.path().join("a/repeated"),
        );
        let result =
            resolve_read(&fs, &virtual_path(&fs, 0, "repeated"), None, &mut budget()).unwrap();
        assert_eq!(result.proof.popped.len(), 1);
        assert!(result.proof.retained_cost() < 16);
        let key = result.proof.popped[0].root.clone();
        let rel = result.proof.popped[0].relative.clone();
        let mut witnesses = Vec::new();
        remember(
            &mut witnesses,
            DirectoryWitness {
                root: key.clone(),
                relative: rel.clone(),
                held: File::open(base.path().join("b/inner")).unwrap(),
            },
        )
        .unwrap();
        remember(
            &mut witnesses,
            DirectoryWitness {
                root: key.clone(),
                relative: rel.clone(),
                held: File::open(base.path().join("b/inner")).unwrap(),
            },
        )
        .unwrap();
        assert_eq!(witnesses.len(), 1);
        std::fs::rename(base.path().join("b/inner"), base.path().join("b/old-inner")).unwrap();
        std::fs::create_dir(base.path().join("b/inner")).unwrap();
        assert!(remember(
            &mut witnesses,
            DirectoryWitness {
                root: key,
                relative: rel,
                held: File::open(base.path().join("b/inner")).unwrap()
            }
        )
        .is_err());
        assert!(result
            .proof
            .validate(&fs, &result.file, &mut budget())
            .is_err());
        let mut full = budget();
        full.retain(READ_DESCRIPTOR_LIMIT).unwrap();
        assert!(
            !resolve_read(&fs, &virtual_path(&fs, 1, &suffix), None, &mut full)
                .err()
                .unwrap()
                .is_unavailable()
        );
        assert_eq!(full.retained, READ_DESCRIPTOR_LIMIT);
    }
    #[test]
    fn readable_links_finite_repeated_alias_keeps_pending_suffix_in_cycle_state() {
        let Some((base, fs)) = fixture() else {
            return;
        };
        link_dir(Path::new("."), &base.path().join("b/inner/alias"));
        let mut resolved = resolve_read(
            &fs,
            &virtual_path(&fs, 1, "inner/alias/alias/data"),
            Some(rpc::Kind::File),
            &mut budget(),
        )
        .unwrap();
        assert_eq!(resolved.proof.links.len(), 2);
        assert!(resolved.read_only);
        let mut bytes = Vec::new();
        resolved.file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"retained target bytes");
        resolved
            .proof
            .validate(&fs, &resolved.file, &mut budget())
            .unwrap();
    }
}
