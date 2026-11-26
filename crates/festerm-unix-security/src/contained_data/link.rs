//! Retained link payloads only; callers still own root/alias containment fences.
use super::*;

/// A metadata-only link pin. Neither the target nor the native handle is public.
/// Revalidation is a sequential snapshot check, not an atomic whole-path proof.
/// The ordinary macOS constructor requires local APFS NODEV; the separate
/// opaque private-parent entry retains and refreshes its complete-chain proof.
pub struct RetainedDataLink {
    parent: File,
    name: CString,
    file: File,
    target: Vec<u8>,
    #[cfg(target_os = "macos")]
    private_parent: Option<PrivateDataParent>,
}
impl RetainedDataLink {
    pub fn target(&self) -> &[u8] {
        &self.target
    }

    /// Check the original no-follow name and payload again. A parent moved out
    /// of a grant must additionally be refused by the caller's rooted fences.
    pub fn revalidate(&self) -> io::Result<()> {
        self.revalidate_checked(&mut || Ok(()))
    }
    /// Debit the caller's current shared budget during every native proof
    /// refresh and leaf operation; no independent pool or callback is retained.
    pub fn revalidate_checked(&self, check: &mut dyn FnMut() -> io::Result<()>) -> io::Result<()> {
        let current = open_link_admitted(
            &self.parent,
            &self.name,
            #[cfg(target_os = "macos")]
            self.private_parent.as_ref(),
            check,
            || {},
        )?;
        check()?;
        if !same_data_object(&self.file, &current)? || payload(&self.file)? != self.target {
            return Err(refused());
        }
        check()?;
        if payload(&current)? != self.target {
            return Err(refused());
        }
        Ok(())
    }
}

/// Pin exactly one link beneath an already retained parent, without following
/// it or opening its target. The returned bytes are not traversal authority.
pub fn pin_data_link(parent: &File, name: &Path) -> io::Result<RetainedDataLink> {
    let name = link_name(name)?;
    let parent = parent.try_clone()?;
    let file = open_link(&parent, &name)?;
    let target = payload(&file)?;
    let pin = RetainedDataLink {
        parent,
        name,
        file,
        target,
        #[cfg(target_os = "macos")]
        private_parent: None,
    };
    pin.revalidate()?;
    Ok(pin)
}

pub(super) fn link_name(name: &Path) -> io::Result<CString> {
    crate::imp::one_component(name)?;
    if name.as_os_str().as_bytes().contains(&b'/') {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    relative(name)
}
#[cfg(target_os = "macos")]
pub(super) fn pin_private_data_link(
    proof: PrivateDataParent,
    resolved_parent: &File,
    name: &Path,
    check: &mut dyn FnMut() -> io::Result<()>,
) -> io::Result<RetainedDataLink> {
    let name = link_name(name)?;
    check()?;
    proof.require_parent(resolved_parent)?;
    let parent = proof.parent_clone()?;
    let file = open_link_admitted(&parent, &name, Some(&proof), check, || {})?;
    check()?;
    let target = payload(&file)?;
    let pin = RetainedDataLink {
        parent,
        name,
        file,
        target,
        private_parent: Some(proof),
    };
    pin.revalidate_checked(check)?;
    Ok(pin)
}

fn open_link(parent: &File, name: &CString) -> io::Result<File> {
    open_link_after_metadata(parent, name, || {})
}
fn open_link_after_metadata(
    parent: &File,
    name: &CString,
    after_metadata: impl FnOnce(),
) -> io::Result<File> {
    open_link_admitted(
        parent,
        name,
        #[cfg(target_os = "macos")]
        None,
        &mut || Ok(()),
        after_metadata,
    )
}
fn open_link_admitted(
    parent: &File,
    name: &CString,
    #[cfg(target_os = "macos")] private_parent: Option<&PrivateDataParent>,
    check: &mut dyn FnMut() -> io::Result<()>,
    after_metadata: impl FnOnce(),
) -> io::Result<File> {
    #[cfg(target_os = "macos")]
    mac_available()?;
    if !parent.metadata()?.is_dir() {
        return Err(refused());
    }
    #[cfg(target_os = "macos")]
    if let Some(proof) = private_parent {
        proof.refresh(check)?;
        check()?;
        proof.require_parent(parent)?;
    } else {
        qualified(parent, true)?;
    }
    #[cfg(target_os = "linux")]
    qualified(parent, false)?;
    check()?;
    let mut initial = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            initial.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let initial = unsafe { initial.assume_init() };
    if initial.st_mode & libc::S_IFMT != libc::S_IFLNK
        || i128::from(initial.st_dev) != i128::from(parent.metadata()?.dev())
    {
        return Err(refused());
    }
    after_metadata();
    check()?;
    #[cfg(target_os = "linux")]
    let file = {
        let mut how: libc::open_how = unsafe { std::mem::zeroed() };
        how.flags = (libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC) as u64;
        how.resolve = libc::RESOLVE_BENEATH
            | libc::RESOLVE_NO_SYMLINKS
            | libc::RESOLVE_NO_MAGICLINKS
            | libc::RESOLVE_NO_XDEV;
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                parent.as_raw_fd(),
                name.as_ptr(),
                &how,
                std::mem::size_of::<libc::open_how>(),
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = owned(fd as i32)?;
        if mount_id(parent)? != mount_id(&file)? {
            return Err(refused());
        }
        file
    };
    #[cfg(target_os = "macos")]
    let file = owned(unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_SYMLINK | libc::O_EVTONLY | libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOCTTY,
        )
    })?;
    let held = file.metadata()?;
    if !held.file_type().is_symlink()
        || i128::from(held.dev()) != i128::from(initial.st_dev)
        || held.ino() != initial.st_ino
        || held.dev() != parent.metadata()?.dev()
    {
        return Err(refused());
    }
    #[cfg(target_os = "macos")]
    if private_parent.is_some() {
        if !ownership_enforcing_data_filesystem(&file)? {
            return Err(refused());
        }
    } else {
        qualified(&file, true)?;
    }
    #[cfg(target_os = "linux")]
    qualified(&file, false)?;
    Ok(file)
}
fn checked_payload(bytes: &[u8]) -> io::Result<Vec<u8>> {
    if bytes.is_empty() || bytes.len() > 4096 || bytes.contains(&0) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(bytes.to_vec())
}
fn payload(file: &File) -> io::Result<Vec<u8>> {
    let mut bytes = [0u8; 4097];
    #[cfg(target_os = "linux")]
    let count = unsafe {
        libc::readlinkat(
            file.as_raw_fd(),
            c"".as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    #[cfg(target_os = "macos")]
    let count = unsafe {
        resolve_freadlink(c"freadlink")?(file.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len())
    };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    let count = usize::try_from(count).map_err(|_| io::ErrorKind::InvalidData)?;
    let captured = bytes.get(..count).ok_or(io::ErrorKind::InvalidData)?;
    checked_payload(captured)
}
#[cfg(target_os = "macos")]
type Freadlink =
    unsafe extern "C" fn(libc::c_int, *mut libc::c_char, libc::size_t) -> libc::ssize_t;
#[cfg(target_os = "macos")]
fn resolve_freadlink(symbol: &std::ffi::CStr) -> io::Result<Freadlink> {
    mac_available()?;
    // A direct libc reference both has the wrong locked binding return type
    // and can add a macOS13 import to binaries that still deploy to macOS11.
    let address = unsafe { libc::dlsym(libc::RTLD_DEFAULT, symbol.as_ptr()) };
    if address.is_null() {
        return Err(io::ErrorKind::Unsupported.into());
    }
    // SAFETY: the sole production name is the public freadlink symbol. The
    // signature matches SDK sys/unistd.h (ssize_t), not libc0.2.189's c_int.
    Ok(unsafe { std::mem::transmute::<*mut libc::c_void, Freadlink>(address) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{symlink, FileTypeExt};

    fn admitted(parent: &File) -> bool {
        #[cfg(target_os = "macos")]
        if let Err(error) = mac_available() {
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
            assert_eq!(
                pin_data_link(parent, Path::new("alias"))
                    .err()
                    .unwrap()
                    .kind(),
                io::ErrorKind::Unsupported
            );
            eprintln!("retained_link: unsupported native prerequisite refused");
            return false;
        }
        #[cfg(target_os = "macos")]
        if let Err(error) = qualified(parent, true) {
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
            assert_eq!(
                pin_data_link(parent, Path::new("alias"))
                    .err()
                    .unwrap()
                    .kind(),
                io::ErrorKind::Unsupported
            );
            eprintln!("retained_link: NODEV native prerequisite refused");
            return false;
        }
        #[cfg(target_os = "linux")]
        if let Err(error) = qualified(parent, false) {
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
            assert_eq!(
                pin_data_link(parent, Path::new("alias"))
                    .err()
                    .unwrap()
                    .kind(),
                io::ErrorKind::Unsupported
            );
            eprintln!("retained_link: unsupported native prerequisite refused");
            return false;
        }
        eprintln!("retained_link: qualified native extraction branch");
        true
    }
    #[test]
    fn retained_link_snapshot_survives_alias_replacement_but_revalidation_refuses() {
        let base = crate::tests::TemporaryDirectory::new();
        let parent = File::open(&base.0).unwrap();
        symlink("target-a", base.0.join("alias")).unwrap();
        if !admitted(&parent) {
            return;
        }
        let pin = pin_data_link(&parent, Path::new("alias")).unwrap();
        assert_eq!(pin.target(), b"target-a");
        pin.revalidate().unwrap();
        pin.revalidate().unwrap();
        std::fs::rename(base.0.join("alias"), base.0.join("retained-a")).unwrap();
        symlink("target-b", base.0.join("alias")).unwrap();
        assert_eq!(pin.target(), b"target-a");
        assert_eq!(payload(&pin.file).unwrap(), b"target-a");
        assert_eq!(
            pin.revalidate().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        let replacement = pin_data_link(&parent, Path::new("alias")).unwrap();
        assert_eq!(replacement.target(), b"target-b");
        replacement.revalidate().unwrap();
        assert!(!same_data_object(&pin.file, &replacement.file).unwrap());
    }
    #[test]
    fn retained_link_preserves_non_utf8_and_never_follows_fifo_target() {
        let base = crate::tests::TemporaryDirectory::new();
        let parent = File::open(&base.0).unwrap();
        symlink("fifo", base.0.join("alias")).unwrap();
        nix::unistd::mkfifo(
            &base.0.join("fifo"),
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        if !admitted(&parent) {
            return;
        }
        let pin = pin_data_link(&parent, Path::new("alias")).unwrap();
        assert_eq!(pin.target(), b"fifo");
        pin.revalidate().unwrap();
        let raw = std::ffi::OsStr::from_bytes(b"target-\xff");
        symlink(raw, base.0.join("raw")).unwrap();
        assert_eq!(
            pin_data_link(&parent, Path::new("raw")).unwrap().target(),
            raw.as_bytes()
        );
        std::fs::write(base.0.join("regular"), b"unchanged").unwrap();
        std::fs::create_dir(base.0.join("directory")).unwrap();
        for name in ["regular", "directory", "fifo"] {
            assert_eq!(
                pin_data_link(&parent, Path::new(name))
                    .err()
                    .unwrap()
                    .kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        assert_eq!(std::fs::read(base.0.join("regular")).unwrap(), b"unchanged");
    }
    #[test]
    fn retained_link_invalid_raw_names_refuse_before_parent_lookup() {
        let base = crate::tests::TemporaryDirectory::new();
        let parent = File::open(&base.0).unwrap();
        for name in [
            "",
            ".",
            "..",
            "/alias",
            "other/alias",
            "alias/.",
            "alias/",
            "bad\0name",
        ] {
            assert_eq!(
                pin_data_link(&parent, Path::new(name))
                    .err()
                    .unwrap()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
        assert_eq!(
            pin_data_link(&parent, Path::new(&"x".repeat(4097)))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(std::fs::read_dir(&base.0).unwrap().count(), 0);
    }
    #[test]
    fn retained_link_payload_limit_refuses_truncation_without_utf8_conversion() {
        assert_eq!(checked_payload(&vec![b'x'; 4096]).unwrap().len(), 4096);
        for bytes in [&b""[..], &b"a\0b"[..], &vec![b'x'; 4097][..]] {
            assert_eq!(
                checked_payload(bytes).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        assert_eq!(checked_payload(b"\xff").unwrap(), b"\xff");
    }
    #[test]
    fn retained_link_raced_fifo_refuses_after_metadata_without_following_or_blocking() {
        let base = crate::tests::TemporaryDirectory::new();
        let parent = File::open(&base.0).unwrap();
        symlink("target-a", base.0.join("alias")).unwrap();
        if !admitted(&parent) {
            return;
        }
        let mut replaced = false;
        let result = open_link_after_metadata(&parent, &CString::new("alias").unwrap(), || {
            std::fs::remove_file(base.0.join("alias")).unwrap();
            nix::unistd::mkfifo(
                &base.0.join("alias"),
                nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
            )
            .unwrap();
            replaced = true;
        });
        assert!(replaced);
        assert_eq!(
            result.err().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(std::fs::symlink_metadata(base.0.join("alias"))
            .unwrap()
            .file_type()
            .is_fifo());
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn retained_link_non_nodev_parent_refuses_before_metadata_open() {
        let base = crate::tests::TemporaryDirectory::new();
        let parent = File::open(&base.0).unwrap();
        symlink("target-a", base.0.join("alias")).unwrap();
        if mac_available().is_ok() && qualified(&parent, true).is_ok() {
            eprintln!(
                "retained_link: normal temp already NODEV, negative mount prerequisite absent"
            );
            return;
        }
        let mut opened = false;
        let result =
            open_link_after_metadata(&parent, &CString::new("alias").unwrap(), || opened = true);
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::Unsupported);
        assert!(!opened);
        assert_eq!(
            std::fs::read_link(base.0.join("alias")).unwrap(),
            Path::new("target-a")
        );
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn retained_link_late_symbol_missing_and_ssize_error_fail_closed() {
        if let Err(error) = mac_available() {
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
            assert!(resolve_freadlink(c"freadlink").is_err());
            return;
        }
        assert_eq!(
            resolve_freadlink(c"festerm_missing_retained_link_symbol")
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::Unsupported
        );
        let base = crate::tests::TemporaryDirectory::new();
        let parent = File::open(&base.0).unwrap();
        let error = payload(&parent).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EINVAL));
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn private_data_parent_link_home_snapshot_and_resolver_parent_match() {
        let Some(base) = private_parent::home_fixture() else {
            return;
        };
        let allowed = base.0.join("allowed");
        let inside = allowed.join("inside");
        std::fs::create_dir(&allowed).unwrap();
        std::fs::create_dir(&inside).unwrap();
        symlink("target-a", inside.join("alias")).unwrap();
        let root = File::open(&allowed).unwrap();
        let parent = File::open(&inside).unwrap();
        assert_eq!(
            pin_data_link(&parent, Path::new("alias"))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::Unsupported,
            "owned actual HOME prerequisite must be nonNODEV"
        );
        let proof = qualify_private_data_parent(&allowed, &root, Path::new("inside"))
            .unwrap()
            .unwrap();
        let wrong = File::open(&allowed).unwrap();
        assert_eq!(
            proof
                .pin_link(&wrong, Path::new("alias"))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            qualify_private_data_parent(&allowed, &parent, Path::new("inside"))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        let proof = qualify_private_data_parent(&allowed, &root, Path::new("inside"))
            .unwrap()
            .unwrap();
        let pin = proof.pin_link(&parent, Path::new("alias")).unwrap();
        assert_eq!(pin.target(), b"target-a");
        pin.revalidate().unwrap();
        std::fs::rename(inside.join("alias"), inside.join("retained-a")).unwrap();
        symlink("target-b", inside.join("alias")).unwrap();
        assert_eq!(pin.target(), b"target-a");
        assert_eq!(payload(&pin.file).unwrap(), b"target-a");
        assert_eq!(
            pin.revalidate().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        eprintln!("private_data_parent: qualified nonNODEV HOME link positive and alias substitution refusal");
        private_parent::finish_home_fixture(base);
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn private_data_parent_refresh_root_parent_and_ancestor_changes_precede_leaf_open() {
        use std::os::unix::fs::PermissionsExt;
        for change in ["root", "parent", "mode"] {
            let Some(base) = private_parent::home_fixture() else {
                return;
            };
            let allowed = base.0.join("allowed");
            let inside = allowed.join("inside");
            std::fs::create_dir(&allowed).unwrap();
            std::fs::create_dir(&inside).unwrap();
            symlink("target-a", inside.join("alias")).unwrap();
            let root = File::open(&allowed).unwrap();
            let parent = File::open(&inside).unwrap();
            let proof = qualify_private_data_parent(&allowed, &root, Path::new("inside"))
                .unwrap()
                .unwrap();
            let pin = proof.pin_link(&parent, Path::new("alias")).unwrap();
            match change {
                "root" => {
                    std::fs::rename(&allowed, base.0.join("moved-root")).unwrap();
                    std::fs::create_dir(&allowed).unwrap();
                }
                "parent" => {
                    std::fs::rename(&inside, base.0.join("moved-parent")).unwrap();
                    std::fs::create_dir(&inside).unwrap();
                }
                "mode" => {
                    std::fs::set_permissions(&allowed, std::fs::Permissions::from_mode(0o777))
                        .unwrap()
                }
                _ => unreachable!(),
            }
            let mut reached_leaf = false;
            let result = open_link_admitted(
                &pin.parent,
                &pin.name,
                pin.private_parent.as_ref(),
                &mut || Ok(()),
                || reached_leaf = true,
            );
            assert!(result.is_err());
            assert!(!reached_leaf, "refresh failure precedes leaf metadata/open");
            assert!(pin.revalidate().is_err());
            assert_eq!(payload(&pin.file).unwrap(), b"target-a");
            if change == "mode" {
                assert!(
                    qualify_private_data_parent(&allowed, &root, Path::new("inside"))
                        .unwrap()
                        .is_none()
                );
                std::fs::set_permissions(&allowed, std::fs::Permissions::from_mode(0o755)).unwrap();
                pin.revalidate().unwrap();
            }
            private_parent::finish_home_fixture(base);
        }
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn private_data_parent_fifo_substitution_is_nonblocking_and_terminal() {
        let Some(base) = private_parent::home_fixture() else {
            return;
        };
        symlink("target-a", base.0.join("alias")).unwrap();
        let root = File::open(&base.0).unwrap();
        let proof = qualify_private_data_parent(&base.0, &root, Path::new(""))
            .unwrap()
            .unwrap();
        let mut swapped = false;
        let result = open_link_admitted(
            &root,
            &CString::new("alias").unwrap(),
            Some(&proof),
            &mut || Ok(()),
            || {
                std::fs::remove_file(base.0.join("alias")).unwrap();
                nix::unistd::mkfifo(
                    &base.0.join("alias"),
                    nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
                )
                .unwrap();
                swapped = true;
            },
        );
        assert!(swapped);
        assert_eq!(
            result.err().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(std::fs::symlink_metadata(base.0.join("alias"))
            .unwrap()
            .file_type()
            .is_fifo());
        private_parent::finish_home_fixture(base);
    }
}
