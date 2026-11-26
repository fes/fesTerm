//! Retained roots for the explicitly negotiated allowlist. A selector is part
//! of each immutable virtual path; pane selection never retargets an object.
use super::*;
use std::path::PathBuf;

pub(super) struct Location {
    pub(super) selector: String,
    path: PathBuf,
    label: rpc::DisplayRoot,
    held: Option<Dir>,
    // Windows locks every absolute-root ancestor without delete sharing.
    _parents: Vec<File>,
}
fn same_inode(a: &File, b: &File) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let a = a.metadata()?;
        let b = b.metadata()?;
        Ok(a.dev() == b.dev() && a.ino() == b.ino())
    }
    #[cfg(windows)]
    {
        festerm_windows_security::same_file_identity(a, b)
    }
}
pub(super) fn same_object(a: &File, b: &File) -> io::Result<bool> {
    #[cfg(unix)]
    {
        festerm_unix_security::same_data_object(a, b)
    }
    #[cfg(windows)]
    {
        festerm_windows_security::same_file_identity(a, b)
    }
}
fn open_root(path: &Path) -> io::Result<(Dir, Vec<File>)> {
    #[cfg(unix)]
    {
        let root = festerm_unix_security::open_absolute_directory_read_nofollow(Path::new("/"))?;
        let relative = path.strip_prefix("/").map_err(|_| invalid())?;
        let file = festerm_unix_security::open_contained_data_directory(&root, relative)?;
        Ok((Dir::from_std_file(file), Vec::new()))
    }
    #[cfg(windows)]
    {
        let (file, parents) = festerm_windows_security::open_absolute_data_directory(path)?;
        Ok((Dir::from_std_file(file), parents))
    }
}
impl Location {
    pub(super) fn prepare(path: &Path) -> io::Result<Self> {
        let (held, parents) = match open_root(path) {
            Ok((root, parents)) => (Some(root), parents),
            Err(_) => (None, Vec::new()),
        };
        Ok(Self {
            selector: uuid::Uuid::new_v4().simple().to_string(),
            path: path.to_owned(),
            label: path
                .to_str()
                .ok_or_else(invalid)?
                .to_owned()
                .try_into()
                .map_err(|_| invalid())?,
            held,
            _parents: parents,
        })
    }
    pub(super) fn same_root(&self, other: &Self) -> io::Result<bool> {
        match (&self.held, &other.held) {
            (Some(a), Some(b)) => same_inode(
                &a.try_clone()?.into_std_file(),
                &b.try_clone()?.into_std_file(),
            ),
            _ => Ok(false),
        }
    }
    pub(super) fn current(&self) -> io::Result<&Dir> {
        let held = self.held.as_ref().ok_or(io::ErrorKind::NotFound)?;
        let (fresh, _parents) = open_root(&self.path)?;
        if !same_object(&fresh.into_std_file(), &held.try_clone()?.into_std_file())? {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(held)
    }
    pub(super) fn metadata(&self) -> rpc::FileLocation {
        rpc::FileLocation {
            selector: self.selector.clone(),
            label: self.label.clone(),
            available: self.current().is_ok(),
            mutations_available: self.strict_root().is_ok(),
        }
    }
    pub(super) fn strict_root(&self) -> io::Result<Dir> {
        let held = self.current()?;
        // Include ancestors OUTSIDE the configured root. A relaxed read root
        // must never become a shortcut into mutation/publication authority.
        let strict = super::open_strict_root_mode(&self.path, true)?;
        if !same_object(
            &strict.try_clone()?.into_std_file(),
            &held.try_clone()?.into_std_file(),
        )? {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(strict)
    }
    pub(super) fn directory(&self, relative: &Path) -> io::Result<(Dir, Vec<File>)> {
        let root = self.current()?.try_clone()?.into_std_file();
        #[cfg(unix)]
        {
            Ok((
                Dir::from_std_file(festerm_unix_security::open_contained_data_directory(
                    &root, relative,
                )?),
                Vec::new(),
            ))
        }
        #[cfg(windows)]
        {
            let (file, parents) =
                festerm_windows_security::open_contained_data_directory(&root, relative)?;
            Ok((Dir::from_std_file(file), parents))
        }
    }
    #[cfg(target_os = "macos")]
    fn private_parent(
        &self,
        relative: &Path,
    ) -> io::Result<Option<festerm_unix_security::PrivateDataParent>> {
        let expected = self.current()?.try_clone()?.into_std_file();
        festerm_unix_security::qualify_private_data_parent(&self.path, &expected, relative)
    }
    pub(super) fn file(&self, relative: &Path) -> io::Result<(File, Vec<File>)> {
        let root = self.current()?.try_clone()?.into_std_file();
        #[cfg(target_os = "macos")]
        if let Some(parent) = self.private_parent(relative.parent().unwrap_or(Path::new("")))? {
            let name = relative.file_name().ok_or_else(invalid)?;
            let file = parent.open_file(Path::new(name))?;
            return Ok((file, Vec::new())); // No ordinary retry after trusted acquisition.
        }
        #[cfg(unix)]
        {
            Ok((
                festerm_unix_security::open_contained_data_file(&root, relative)?,
                Vec::new(),
            ))
        }
        #[cfg(windows)]
        {
            festerm_windows_security::open_contained_data_file(&root, relative)
        }
    }
    pub(super) fn virtual_root(&self) -> String {
        format!("/{}", self.selector)
    }
}
pub(super) fn prepare(policy: &rpc::AllowedLocations) -> io::Result<Vec<Location>> {
    let mut roots: Vec<Location> = Vec::new();
    for path in &policy.roots {
        let mut root = Location::prepare(path)?;
        for old in &mut roots {
            if root.same_root(old).unwrap_or(true) {
                root.held = None;
                old.held = None;
            }
        }
        roots.push(root);
    }
    Ok(roots)
}
pub(super) fn selected<'a>(
    roots: &'a [Location],
    path: &str,
) -> io::Result<(&'a Location, String)> {
    let mut parts = names(path)?.into_iter();
    let selector = parts.next().ok_or_else(invalid)?;
    let root = roots
        .iter()
        .find(|root| root.selector == selector)
        .ok_or_else(invalid)?;
    Ok((root, parts.collect::<Vec<_>>().join("/")))
}
pub(super) fn check_directory(filesystem: &Filesystem, path: &str, held: &File) -> io::Result<()> {
    let (fresh, _parents) = filesystem.read_directory(path)?;
    if !same_object(&fresh.into_std_file(), held)? {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}
pub(super) fn check_file(filesystem: &Filesystem, path: &str, held: &File) -> io::Result<()> {
    let (fresh, _parents) = filesystem.read_file(path)?;
    if !same_object(&fresh, held)? {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}
