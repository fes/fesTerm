//! Complete macOS private-chain proof; no public descriptor conversion.
use super::*;
use std::path::PathBuf;

type Check<'a> = &'a mut dyn FnMut() -> io::Result<()>;

/// Exact, refreshable private-data source parent. Only native leaf operations
/// consume this proof; it grants no target traversal or transfer authority.
pub struct PrivateDataParent {
    configured_root: PathBuf,
    relative_parent: PathBuf,
    root: File,
    parent: File,
}
pub fn qualify_private_data_parent(
    configured_root: &Path,
    held_root: &File,
    relative_parent: &Path,
) -> io::Result<Option<PrivateDataParent>> {
    qualify_private_data_parent_checked(configured_root, held_root, relative_parent, &mut || Ok(()))
}
/// Each checked step debits the caller's existing work/deadline/cancellation
/// budget, including every component of independent complete lookups.
pub fn qualify_private_data_parent_checked(
    configured_root: &Path,
    held_root: &File,
    relative_parent: &Path,
    check: Check<'_>,
) -> io::Result<Option<PrivateDataParent>> {
    validate_inputs(configured_root, relative_parent)?;
    check()?;
    crate::current_user_id()?;
    mac_available()?;
    let slash = crate::open_absolute_directory_read_nofollow(Path::new("/"))?;
    let fresh = current_root(&slash, configured_root, check)?;
    if !same_data_object(&fresh, held_root)? {
        return Err(refused());
    }
    let root_depth = configured_root
        .components()
        .filter(|p| matches!(p, Component::Normal(_)))
        .count();
    let parts = configured_root
        .components()
        .chain(relative_parent.components())
        .filter_map(|p| {
            if let Component::Normal(name) = p {
                Some(name)
            } else {
                None
            }
        });
    let mut directory = slash.try_clone()?;
    for (depth, part) in parts.enumerate() {
        if depth == root_depth && !same_data_object(&directory, held_root)? {
            return Err(refused());
        }
        if !trusted(&directory, check)? {
            return Ok(None);
        }
        check()?;
        directory = crate::open_directory_read_nofollow(&directory, Path::new(part))?;
    }
    if relative_parent.as_os_str().is_empty() && !same_data_object(&directory, held_root)? {
        return Err(refused());
    }
    if !trusted(&directory, check)? {
        return Ok(None);
    }
    // The complete root and contained-parent lookups are independent from the
    // walk, matching the former Location::current + directory fences.
    let fresh = current_root(&slash, configured_root, check)?;
    if !same_data_object(&fresh, held_root)? {
        return Err(refused());
    }
    charge_path(relative_parent, check)?;
    let contained = open_contained_data_directory(&fresh, relative_parent)?;
    if !same_data_object(&contained, &directory)? {
        return Err(refused());
    }
    Ok(Some(PrivateDataParent {
        configured_root: configured_root.to_owned(),
        relative_parent: relative_parent.to_owned(),
        root: held_root.try_clone()?,
        parent: directory,
    }))
}
fn trusted(directory: &File, check: Check<'_>) -> io::Result<bool> {
    check()?;
    if !ownership_enforcing_data_filesystem(directory)?
        || !crate::private_storage_ancestor_qualified(directory)?
    {
        return Ok(false);
    }
    crate::trusted_private_storage_ancestor(directory)?;
    Ok(true)
}
fn charge_path(path: &Path, check: Check<'_>) -> io::Result<()> {
    check()?; // Root/empty lookup also has a native cost.
    for part in path.components() {
        if matches!(part, Component::Normal(_)) {
            check()?;
        }
    }
    Ok(())
}
fn current_root(slash: &File, configured: &Path, check: Check<'_>) -> io::Result<File> {
    charge_path(configured, check)?;
    open_contained_data_directory(
        slash,
        configured
            .strip_prefix("/")
            .map_err(|_| io::ErrorKind::InvalidInput)?,
    )
}
fn validate_inputs(root: &Path, relative: &Path) -> io::Result<()> {
    let root = root.as_os_str().as_bytes();
    let rel = relative.as_os_str().as_bytes();
    let invalid = || io::Error::from(io::ErrorKind::InvalidInput);
    if root.is_empty()
        || root.len() > 4096
        || rel.len() > 4096
        || root[0] != b'/'
        || root.contains(&0)
        || rel.contains(&0)
    {
        return Err(invalid());
    }
    let body = &root[1..];
    let body = body.strip_suffix(b"/").unwrap_or(body);
    if !body.is_empty()
        && body
            .split(|b| *b == b'/')
            .any(|p| p.is_empty() || p == b"." || p == b"..")
    {
        return Err(invalid());
    }
    if root != b"/" && body.is_empty() {
        return Err(invalid());
    }
    if !rel.is_empty()
        && rel
            .split(|b| *b == b'/')
            .any(|p| p.is_empty() || p == b"." || p == b"..")
    {
        return Err(invalid());
    }
    Ok(())
}
impl PrivateDataParent {
    pub fn open_file(&self, name: &Path) -> io::Result<File> {
        self.open_file_checked(name, &mut || Ok(()))
    }
    pub fn open_file_checked(&self, name: &Path, check: Check<'_>) -> io::Result<File> {
        link::link_name(name)?;
        self.refresh(check)?;
        check()?;
        let file = open_trusted_data_file(&self.parent, name)?;
        self.refresh(check)?;
        Ok(file)
    }
    pub fn pin_link(self, resolved_parent: &File, name: &Path) -> io::Result<RetainedDataLink> {
        self.pin_link_checked(resolved_parent, name, &mut || Ok(()))
    }
    pub fn pin_link_checked(
        self,
        resolved_parent: &File,
        name: &Path,
        check: Check<'_>,
    ) -> io::Result<RetainedDataLink> {
        link::pin_private_data_link(self, resolved_parent, name, check)
    }
    pub(super) fn refresh(&self, check: Check<'_>) -> io::Result<()> {
        let fresh = qualify_private_data_parent_checked(
            &self.configured_root,
            &self.root,
            &self.relative_parent,
            check,
        )?
        .ok_or_else(refused)?;
        if !same_data_object(&fresh.parent, &self.parent)? {
            return Err(refused());
        }
        Ok(())
    }
    pub(super) fn require_parent(&self, resolved_parent: &File) -> io::Result<()> {
        if !same_data_object(&self.parent, resolved_parent)? {
            return Err(refused());
        }
        Ok(())
    }
    pub(super) fn parent_clone(&self) -> io::Result<File> {
        self.parent.try_clone()
    }
}

#[cfg(test)]
pub(super) fn home_fixture() -> Option<crate::tests::TemporaryDirectory> {
    if let Err(error) = mac_available() {
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        let slash = crate::open_absolute_directory_read_nofollow(Path::new("/")).unwrap();
        assert_eq!(
            qualify_private_data_parent(Path::new("/"), &slash, Path::new(""))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::Unsupported
        );
        eprintln!("private_data_parent: unsupported macOS floor, native positive not run");
        return None;
    }
    crate::current_user_id().unwrap();
    let home = crate::current_user_home().unwrap();
    let mut bytes = home
        .join(".festerm-private-link-XXXXXX")
        .as_os_str()
        .as_bytes()
        .to_vec();
    bytes.push(0);
    let created = unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast()) };
    assert!(!created.is_null(), "owned HOME fixture creation");
    let name = unsafe { std::ffi::CStr::from_ptr(created) }.to_bytes();
    use std::os::unix::ffi::OsStringExt;
    let path = PathBuf::from(std::ffi::OsString::from_vec(name.to_vec()));
    let metadata = std::fs::symlink_metadata(&path).unwrap();
    assert_eq!(metadata.mode() & 0o777, 0o700);
    assert_eq!(metadata.uid(), crate::current_user_id().unwrap());
    Some(crate::tests::TemporaryDirectory(path))
}
#[cfg(test)]
pub(super) fn finish_home_fixture(base: crate::tests::TemporaryDirectory) {
    let path = base.0.clone();
    drop(base); // Reuse the existing owned-directory fixture cleanup.
    assert_eq!(
        std::fs::symlink_metadata(path).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_data_parent_inputs_keep_independent_bounds_and_root_separator() {
        for root in ["relative", "//", "/a//b", "/a/./b", "/a/../b", "/a\0b"] {
            assert_eq!(
                validate_inputs(Path::new(root), Path::new(""))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
        for relative in ["/a", ".", "..", "a/.", "a/../b", "a//b", "a/", "a\0b"] {
            assert_eq!(
                validate_inputs(Path::new("/"), Path::new(relative))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
        validate_inputs(Path::new("/"), Path::new("")).unwrap();
        validate_inputs(Path::new("/root/"), Path::new("inside")).unwrap();
        let root = format!("/{}a", "a/".repeat(2047));
        let relative = format!("{}bb", "b/".repeat(2047));
        assert_eq!((root.len(), relative.len()), (4096, 4096));
        validate_inputs(Path::new(&root), Path::new(&relative)).unwrap();
        assert!(validate_inputs(Path::new(&(root.clone() + "a")), Path::new("")).is_err());
        assert!(validate_inputs(Path::new("/"), Path::new(&(relative + "b"))).is_err());
    }
    #[test]
    fn private_data_parent_regular_home_read_and_checked_budget_are_terminal() {
        use std::io::Read;
        use std::os::unix::fs::PermissionsExt;
        let Some(base) = home_fixture() else {
            return;
        };
        let allowed = base.0.join("allowed");
        let inside = allowed.join("inside");
        std::fs::create_dir(&allowed).unwrap();
        std::fs::create_dir(&inside).unwrap();
        let root = File::open(&allowed).unwrap();
        std::fs::write(inside.join("owned"), b"private regular original").unwrap();
        let configured = PathBuf::from(format!("{}/", allowed.display()));
        let mut steps = 0usize;
        let proof = qualify_private_data_parent_checked(
            &configured,
            &root,
            Path::new("inside"),
            &mut || {
                steps += 1;
                Ok(())
            },
        )
        .unwrap()
        .unwrap();
        let root_components = configured
            .components()
            .filter(|p| matches!(p, Component::Normal(_)))
            .count();
        assert!(
            steps >= root_components * 3 + 2,
            "all complete lookups must be charged"
        );
        let mut bytes = Vec::new();
        proof
            .open_file(Path::new("owned"))
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"private regular original");
        assert_eq!(
            proof
                .open_file_checked(Path::new("owned"), &mut || Err(
                    io::ErrorKind::TimedOut.into()
                ))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(
            qualify_private_data_parent_checked(
                &configured,
                &root,
                Path::new("inside"),
                &mut || Err(io::ErrorKind::Interrupted.into())
            )
            .err()
            .unwrap()
            .kind(),
            io::ErrorKind::Interrupted
        );
        std::fs::set_permissions(inside.join("owned"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        let denied = proof.open_file(Path::new("owned"));
        std::fs::set_permissions(inside.join("owned"), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        assert_eq!(
            denied.err().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            std::fs::read(inside.join("owned")).unwrap(),
            b"private regular original"
        );
        eprintln!("private_data_parent: qualified HOME regular positive/trailing-root/budget/denial controls");
        finish_home_fixture(base);
    }
}
