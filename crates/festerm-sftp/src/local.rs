//! The "This iPhone" pane: a sandbox root (Documents, a picked folder, ...)
//! with the same listing model as the remote pane.
//!
//! Confinement: every path is a relative, validated component list. Paths are
//! canonicalized and must stay under the canonical root. Symlinks are listed
//! and may be followed only while their final target stays inside the root;
//! a link that leaves the root is listed with `escapes_root` and refused for
//! reads, listing and transfers. Mutations (`delete`, `rename`) act on the
//! link itself, never on its target.

use crate::{
    error::{Result, SftpError},
    fs::{PaneFs, PaneKind, MAX_DELETE_DEPTH, MAX_DELETE_ENTRIES},
    model::{finish_listing, Entry, EntryKind, ListOptions, Listing, SymlinkInfo},
    path::{self, local_components},
};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(crate) const PARTIAL_PREFIX: &str = ".festerm-";
pub(crate) const PARTIAL_SUFFIX: &str = ".part";

#[derive(Clone, Debug)]
pub struct LocalFs {
    root: PathBuf,
}

fn io_error(error: &std::io::Error, context: &str) -> SftpError {
    SftpError::from_io(error, context)
}

pub(crate) fn unix_seconds(time: std::io::Result<SystemTime>) -> Option<i64> {
    let time = time.ok()?;
    Some(match time.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_secs()).ok()?,
        Err(before) => -i64::try_from(before.duration().as_secs()).ok()?,
    })
}

fn kind_of(file_type: fs::FileType) -> EntryKind {
    if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_dir() {
        EntryKind::Directory
    } else if file_type.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

fn permissions_of(metadata: &fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(metadata.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

impl LocalFs {
    /// `root` must be an existing directory; it is canonicalized once.
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = fs::canonicalize(root.as_ref()).map_err(|e| io_error(&e, "sandbox root"))?;
        if !root.is_dir() {
            return Err(SftpError::NotADirectory("sandbox root".into()));
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn within(&self, candidate: &Path) -> bool {
        candidate.starts_with(&self.root)
    }

    fn lexical(&self, relative: &str) -> Result<PathBuf> {
        let mut path = self.root.clone();
        for component in local_components(relative)? {
            path.push(component);
        }
        Ok(path)
    }

    /// Canonical path of an existing entry, following symlinks, confined to the root.
    pub(crate) fn resolve_follow(&self, relative: &str) -> Result<PathBuf> {
        let lexical = self.lexical(relative)?;
        let canonical = fs::canonicalize(&lexical).map_err(|e| io_error(&e, relative))?;
        if self.within(&canonical) {
            Ok(canonical)
        } else {
            Err(SftpError::SymlinkRefused(format!(
                "{relative} resolves outside the sandbox"
            )))
        }
    }

    /// Absolute path of the entry itself: the parent is canonicalized and
    /// confined, the last component is not followed.
    pub(crate) fn resolve_entry(&self, relative: &str) -> Result<PathBuf> {
        let components = local_components(relative)?;
        let Some((last, parents)) = components.split_last() else {
            return Err(SftpError::InvalidPath(
                "the sandbox root itself cannot be targeted".into(),
            ));
        };
        let mut parent = self.root.clone();
        for component in parents {
            parent.push(component);
        }
        let parent = fs::canonicalize(&parent).map_err(|e| io_error(&e, relative))?;
        if !self.within(&parent) {
            return Err(SftpError::SymlinkRefused(format!(
                "{relative} is reached through a link leaving the sandbox"
            )));
        }
        Ok(parent.join(last))
    }

    fn symlink_info(&self, path: &Path) -> (SymlinkInfo, Option<fs::Metadata>) {
        let target = fs::read_link(path)
            .ok()
            .map(|t| t.to_string_lossy().into_owned());
        let unresolved = |escapes_root| SymlinkInfo {
            target_kind: None,
            target: target.clone(),
            escapes_root,
        };
        match fs::canonicalize(path) {
            Ok(canonical) if self.within(&canonical) => match fs::metadata(&canonical) {
                Ok(metadata) => (
                    SymlinkInfo {
                        target_kind: Some(kind_of(metadata.file_type())),
                        target,
                        escapes_root: false,
                    },
                    Some(metadata),
                ),
                Err(_) => (unresolved(false), None),
            },
            Ok(_) => (unresolved(true), None),
            Err(_) => (unresolved(false), None),
        }
    }

    fn build_entry(&self, name: String, safe_utf8: bool, path: &Path) -> Result<Entry> {
        let metadata = fs::symlink_metadata(path).map_err(|e| io_error(&e, &name))?;
        let kind = kind_of(metadata.file_type());
        let (symlink, followed) = if kind == EntryKind::Symlink {
            let (info, followed) = self.symlink_info(path);
            (Some(info), followed)
        } else {
            (None, None)
        };
        let shown = followed.as_ref().unwrap_or(&metadata);
        Ok(Entry {
            display_name: path::display_name(&name),
            hidden: name.starts_with('.'),
            name_safe: safe_utf8 && path::name_problem(&name).is_none(),
            kind,
            size: (kind == EntryKind::File || followed.is_some()).then_some(shown.len()),
            modified: unix_seconds(shown.modified()),
            permissions: permissions_of(&metadata),
            symlink,
            name,
        })
    }

    pub fn stat_entry(&self, relative: &str) -> Result<Entry> {
        let components = local_components(relative)?;
        if components.is_empty() {
            let mut entry = self.build_entry(String::new(), false, &self.root)?;
            entry.display_name = "/".into();
            return Ok(entry);
        }
        let path = self.resolve_entry(relative)?;
        let name = components[components.len() - 1].to_owned();
        self.build_entry(name, true, &path)
    }

    /// Entry named `name` inside directory `directory`, or `None`.
    pub(crate) fn existing(&self, directory: &Path, name: &str) -> Result<Option<Entry>> {
        let path = directory.join(name);
        match fs::symlink_metadata(&path) {
            Ok(_) => self.build_entry(name.to_owned(), true, &path).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_error(&error, name)),
        }
    }

    pub(crate) fn resolve_dir(&self, relative: &str) -> Result<PathBuf> {
        let path = self.resolve_follow(relative)?;
        if path.is_dir() {
            Ok(path)
        } else {
            Err(SftpError::NotADirectory(relative.to_owned()))
        }
    }

    /// Opens a confined regular file for reading. A symlink is followed only
    /// when its target stays inside the root.
    pub(crate) fn open_source(&self, relative: &str) -> Result<(fs::File, fs::Metadata)> {
        let path = self.resolve_follow(relative)?;
        let file = fs::File::open(&path).map_err(|e| io_error(&e, relative))?;
        let metadata = file.metadata().map_err(|e| io_error(&e, relative))?;
        if !metadata.is_file() {
            return Err(if metadata.is_dir() {
                SftpError::IsADirectory(relative.to_owned())
            } else {
                SftpError::Unsupported(format!("{relative} is not a regular file"))
            });
        }
        Ok((file, metadata))
    }

    /// Removes leftover `.festerm-*.part` files older than `older_than`
    /// (left behind when the app was terminated mid-transfer) in one
    /// directory; returns how many were removed.
    pub fn remove_stale_partials(&self, directory: &str, older_than: Duration) -> Result<usize> {
        let dir = self.resolve_dir(directory)?;
        let mut removed = 0;
        let now = SystemTime::now();
        for item in fs::read_dir(&dir)
            .map_err(|e| io_error(&e, directory))?
            .take(crate::model::ABSOLUTE_MAX_SCAN)
            .flatten()
        {
            let name = item.file_name();
            let Some(name) = name.to_str() else { continue };
            if !(name.starts_with(PARTIAL_PREFIX) && name.ends_with(PARTIAL_SUFFIX)) {
                continue;
            }
            let Ok(metadata) = item.metadata() else {
                continue;
            };
            let old = metadata
                .modified()
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .is_some_and(|age| age >= older_than);
            if metadata.is_file() && old && fs::remove_file(item.path()).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn count_tree(&self, path: &Path, depth: usize, budget: &mut usize) -> Result<()> {
        if depth > MAX_DELETE_DEPTH {
            return Err(SftpError::LimitExceeded("directory tree too deep".into()));
        }
        for item in fs::read_dir(path).map_err(|e| io_error(&e, "delete"))? {
            let item = item.map_err(|e| io_error(&e, "delete"))?;
            *budget = budget.checked_sub(1).ok_or_else(|| {
                SftpError::LimitExceeded("too many entries to delete at once".into())
            })?;
            let file_type = item.file_type().map_err(|e| io_error(&e, "delete"))?;
            if file_type.is_dir() {
                self.count_tree(&item.path(), depth + 1, budget)?;
            }
        }
        Ok(())
    }

    fn remove_tree(&self, path: &Path) -> Result<()> {
        for item in fs::read_dir(path).map_err(|e| io_error(&e, "delete"))? {
            let item = item.map_err(|e| io_error(&e, "delete"))?;
            let file_type = item.file_type().map_err(|e| io_error(&e, "delete"))?;
            if file_type.is_dir() {
                self.remove_tree(&item.path())?;
                fs::remove_dir(item.path()).map_err(|e| io_error(&e, "delete"))?;
            } else {
                // Links are unlinked, never followed.
                fs::remove_file(item.path()).map_err(|e| io_error(&e, "delete"))?;
            }
        }
        Ok(())
    }
}

impl PaneFs for LocalFs {
    fn kind(&self) -> PaneKind {
        PaneKind::Local
    }

    fn home(&self) -> String {
        String::new()
    }

    fn list(&self, relative: &str, options: &ListOptions) -> Result<Listing> {
        let directory = self.resolve_dir(relative)?;
        let limit = options.scan_limit();
        let mut entries = Vec::new();
        let mut truncated = false;
        for item in fs::read_dir(&directory).map_err(|e| io_error(&e, relative))? {
            let Ok(item) = item else { continue };
            if entries.len() >= limit {
                truncated = true;
                break;
            }
            let raw = item.file_name();
            let (name, utf8) = match raw.to_str() {
                Some(name) => (name.to_owned(), true),
                None => (raw.to_string_lossy().into_owned(), false),
            };
            if let Ok(entry) = self.build_entry(name, utf8, &item.path()) {
                entries.push(entry);
            }
        }
        Ok(finish_listing(
            path::normalize_local(relative)?,
            entries,
            truncated,
            options,
        ))
    }

    fn stat(&self, relative: &str) -> Result<Entry> {
        self.stat_entry(relative)
    }

    fn mkdir(&self, relative: &str) -> Result<()> {
        let path = self.resolve_entry(relative)?;
        fs::create_dir(&path).map_err(|e| io_error(&e, relative))
    }

    fn rename(&self, from: &str, to: &str) -> Result<()> {
        let source = self.resolve_entry(from)?;
        let target = self.resolve_entry(to)?;
        if fs::symlink_metadata(&target).is_ok() {
            return Err(SftpError::AlreadyExists(to.to_owned()));
        }
        fs::rename(&source, &target).map_err(|e| io_error(&e, from))
    }

    fn delete(&self, relative: &str, recursive: bool) -> Result<()> {
        let path = self.resolve_entry(relative)?;
        let metadata = fs::symlink_metadata(&path).map_err(|e| io_error(&e, relative))?;
        if !metadata.is_dir() {
            return fs::remove_file(&path).map_err(|e| io_error(&e, relative));
        }
        if recursive {
            let mut budget = MAX_DELETE_ENTRIES;
            self.count_tree(&path, 1, &mut budget)?;
            self.remove_tree(&path)?;
        }
        fs::remove_dir(&path).map_err(|e| io_error(&e, relative))
    }

    fn join(&self, directory: &str, name: &str) -> Result<String> {
        path::join_local(directory, name)
    }

    fn parent(&self, path: &str) -> Option<String> {
        path::parent_local(path)
    }
}
