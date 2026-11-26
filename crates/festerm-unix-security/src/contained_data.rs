//! HOST's explicitly reviewed ordinary-data reads. Private state/publication
//! continue to use the existing strict primitives in lib.rs.
use nix::libc;
use std::{
    ffi::CString,
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
};
fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
fn relative(path: &Path) -> io::Result<CString> {
    if path.as_os_str().len() > 4096
        || path
            .components()
            .any(|p| !matches!(p, Component::Normal(_)))
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    CString::new(if path.as_os_str().is_empty() {
        b".".as_slice()
    } else {
        path.as_os_str().as_bytes()
    })
    .map_err(|_| io::ErrorKind::InvalidInput.into())
}
fn owned(fd: i32) -> io::Result<File> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}
/// Exact new-scope identity. A Linux bind cover can preserve dev/inode while
/// changing the mount view; unavailable mount identity refuses this namespace.
pub fn same_data_object(a: &File, b: &File) -> io::Result<bool> {
    let left = a.metadata()?;
    let right = b.metadata()?;
    let same_inode = left.dev() == right.dev() && left.ino() == right.ino();
    #[cfg(target_os = "linux")]
    {
        Ok(same_inode && mount_id(a)? == mount_id(b)?)
    }
    #[cfg(target_os = "macos")]
    {
        Ok(same_inode)
    }
}
#[cfg(target_os = "linux")]
fn mount_id(file: &File) -> io::Result<u64> {
    let mut value = std::mem::MaybeUninit::<libc::statx>::uninit();
    if unsafe {
        libc::syscall(
            libc::SYS_statx,
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_MNT_ID,
            value.as_mut_ptr(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let value = unsafe { value.assume_init() };
    if value.stx_mask & libc::STATX_MNT_ID == 0 || value.stx_mnt_id == 0 {
        return Err(io::ErrorKind::Unsupported.into());
    }
    Ok(value.stx_mnt_id)
}
/// New allowlist mutation traversal only. Legacy/private-store semantics stay
/// in the unchanged trusted_private_storage_ancestor primitive.
pub fn qualify_data_mutation_ancestor(file: &File) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    if !ownership_enforcing_data_filesystem(file)? {
        return Err(refused());
    }
    #[cfg(target_os = "linux")]
    {
        qualified(file, false)?;
        mount_id(file)?;
    }
    crate::trusted_private_storage_ancestor(file)
}
#[cfg(target_os = "linux")]
fn current_proc_fd_directory() -> io::Result<File> {
    let root = crate::open_absolute_directory_read_nofollow(Path::new("/proc"))?;
    let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::fstatfs(root.as_raw_fd(), fs.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { fs.assume_init() }.f_type != libc::PROC_SUPER_MAGIC {
        return Err(refused());
    }
    // self is a kernel-controlled relative link in this retained procfs. Resolve
    // it there, not via getpid from a potentially different PID namespace.
    // No mount crossing or user-provided link/path is admitted.
    let mut how: libc::open_how = unsafe { std::mem::zeroed() };
    how.flags = (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64;
    how.resolve = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_XDEV | libc::RESOLVE_NO_MAGICLINKS;
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            root.as_raw_fd(),
            c"self/fd".as_ptr(),
            &how,
            std::mem::size_of::<libc::open_how>(),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let directory = owned(fd as i32)?;
    if mount_id(&root)? != mount_id(&directory)? {
        return Err(refused());
    }
    Ok(directory)
}

#[cfg(target_os = "macos")]
fn mac_available() -> io::Result<()> {
    // These flags are qualified only on the recorded deployment floor. Older
    // kernels must not silently ignore an unfamiliar open flag.
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *AVAILABLE.get_or_init(|| {
        let mut bytes = [0u8; 64];
        let mut size = bytes.len();
        let result = unsafe {
            libc::sysctlbyname(
                c"kern.osproductversion".as_ptr(),
                bytes.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 || size == 0 || size > bytes.len() {
            return false;
        }
        let Ok(text) = std::str::from_utf8(&bytes[..size.saturating_sub(1)]) else {
            return false;
        };
        let mut values = [0u32; 3];
        for (index, part) in text.split('.').enumerate() {
            if index >= 3 {
                return false;
            }
            let Ok(value) = part.parse() else {
                return false;
            };
            values[index] = value;
        }
        values >= [27, 2, 1]
    }) {
        Ok(())
    } else {
        Err(io::ErrorKind::Unsupported.into())
    }
}
#[cfg(target_os = "macos")]
fn qualified(file: &File, data: bool) -> io::Result<()> {
    let mut value = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::fstatfs(file.as_raw_fd(), value.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let value = unsafe { value.assume_init() };
    let kind = unsafe { std::ffi::CStr::from_ptr(value.f_fstypename.as_ptr()) };
    if kind.to_bytes() != b"apfs"
        || value.f_flags & (libc::MNT_LOCAL as u32) == 0
        || data && value.f_flags & (libc::MNT_NODEV as u32) == 0
    {
        return Err(io::ErrorKind::Unsupported.into());
    }
    Ok(())
}
#[cfg(target_os = "linux")]
fn supported_data_class(kind: u64) -> bool {
    [0xef53, 0x58465342, 0x9123683e, 0x01021994].contains(&kind)
}
#[cfg(target_os = "linux")]
fn qualified(file: &File, _data: bool) -> io::Result<()> {
    let mut value = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::fstatfs(file.as_raw_fd(), value.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let kind = unsafe { value.assume_init() }.f_type as u64;
    // Explicit kernel-backed local classes. No FUSE/network/pseudo/overlay
    // fallback is inferred from a broad path or a successful open.
    if !supported_data_class(kind) {
        return Err(io::ErrorKind::Unsupported.into());
    }
    Ok(())
}
#[cfg(target_os = "linux")]
fn linux_open(root: &File, path: &CString, flags: i32) -> io::Result<File> {
    let mut how: libc::open_how = unsafe { std::mem::zeroed() };
    how.flags = flags as u64;
    how.resolve = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_MAGICLINKS;
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            root.as_raw_fd(),
            path.as_ptr(),
            &how,
            std::mem::size_of::<libc::open_how>(),
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        owned(fd as i32)
    }
}
/// New Linux mutation/review leaf only. Directory traversal has already
/// admitted its strict parent; this lookup must never cross a mounted leaf.
#[cfg(target_os = "linux")]
pub fn open_mutation_file_nofollow(parent: &File, name: &Path) -> io::Result<File> {
    crate::imp::one_component(name)?;
    if name.as_os_str().as_bytes().contains(&b'/') {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let name = relative(name)?;
    qualify_data_mutation_ancestor(parent)?;
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
    let pinned = owned(fd as i32)?;
    if !pinned.metadata()?.is_file() {
        return Err(refused());
    }
    qualified(&pinned, true)?;
    if mount_id(parent)? != mount_id(&pinned)? {
        return Err(refused());
    }
    reopen_pinned_regular(&pinned)
}
#[cfg(all(test, target_os = "linux"))]
std::thread_local! {static REGULAR_DATA_OPENS:std::cell::Cell<usize>=const {std::cell::Cell::new(0)};}
#[cfg(target_os = "linux")]
fn reopen_pinned_regular(metadata: &File) -> io::Result<File> {
    if !metadata.metadata()?.is_file() {
        return Err(refused());
    }
    qualified(metadata, true)?;
    // Only the already-qualified current-process regular descriptor is
    // followed here. User pathnames never enter this procfs lookup.
    let proc = current_proc_fd_directory()?;
    let fd = CString::new(metadata.as_raw_fd().to_string()).map_err(|_| refused())?;
    #[cfg(test)]
    REGULAR_DATA_OPENS.with(|count| count.set(count.get() + 1));
    let file = owned(unsafe {
        libc::openat(
            proc.as_raw_fd(),
            fd.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    })?;
    if !file.metadata()?.is_file() || !same_data_object(metadata, &file)? {
        return Err(refused());
    }
    qualified(&file, true)?;
    Ok(file)
}

/// Complete-root lookup; no descendant shortcut or ambient pathname fallback.
pub fn open_contained_data_directory(root: &File, path: &Path) -> io::Result<File> {
    crate::current_user_id()?;
    let path = relative(path)?;
    #[cfg(target_os = "macos")]
    let file = {
        mac_available()?;
        owned(unsafe {
            libc::openat(
                root.as_raw_fd(),
                path.as_ptr(),
                libc::O_RDONLY
                    | libc::O_DIRECTORY
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW_ANY
                    | 0x00001000,
            )
        })?
    };
    #[cfg(target_os = "linux")]
    let file = linux_open(
        root,
        &path,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
    )?;
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (root, path);
        return Err(io::ErrorKind::Unsupported.into());
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        if !file.metadata()?.is_dir() {
            return Err(refused());
        }
        qualified(&file, false)?;
        #[cfg(target_os = "linux")]
        mount_id(&file)?;
        Ok(file)
    }
}
/// Regular-file admission precedes bytes. A rejected raced FIFO on macOS can
/// have been opened nonblocking; this is not an atomic pre-open type guarantee.
pub fn open_contained_data_file(root: &File, path: &Path) -> io::Result<File> {
    crate::current_user_id()?;
    let name = relative(path)?;
    #[cfg(target_os = "macos")]
    {
        mac_available()?;
        let parent = open_contained_data_directory(root, path.parent().unwrap_or(Path::new("")))?;
        qualified(&parent, true)?;
        let mut before = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SDK sys/fcntl.h: AT_SYMLINK_NOFOLLOW_ANY | AT_RESOLVE_BENEATH.
        if unsafe {
            libc::fstatat(
                root.as_raw_fd(),
                name.as_ptr(),
                before.as_mut_ptr(),
                0x0800 | 0x2000,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let before = unsafe { before.assume_init() };
        if before.st_mode & libc::S_IFMT != libc::S_IFREG
            || before.st_dev as u64 != parent.metadata()?.dev()
        {
            return Err(refused());
        }
        // SDK O_RESOLVE_BENEATH=0x1000, not a portable libc fallback.
        let file = owned(unsafe {
            libc::openat(
                root.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY
                    | libc::O_NONBLOCK
                    | libc::O_NOCTTY
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW_ANY
                    | 0x00001000,
            )
        })?;
        let after = file.metadata()?;
        if !after.is_file() || after.dev() != before.st_dev as u64 || after.ino() != before.st_ino {
            return Err(refused());
        }
        qualified(&file, true)?;
        Ok(file)
    }
    #[cfg(target_os = "linux")]
    {
        // O_PATH pins an object without invoking a device/FIFO data open.
        let metadata = linux_open(
            root,
            &name,
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )?;
        reopen_pinned_regular(&metadata)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (root, name);
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(target_os = "macos")]
fn ownership_enforcing_apfs(kind: &[u8], flags: u32) -> bool {
    kind == b"apfs"
        && flags & (libc::MNT_LOCAL as u32) != 0
        && flags & (libc::MNT_IGNORE_OWNERSHIP as u32) == 0
}
#[cfg(target_os = "macos")]
pub fn ownership_enforcing_data_filesystem(file: &File) -> io::Result<bool> {
    mac_available()?;
    let mut value = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::fstatfs(file.as_raw_fd(), value.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let value = unsafe { value.assume_init() };
    let kind = unsafe { std::ffi::CStr::from_ptr(value.f_fstypename.as_ptr()) };
    Ok(ownership_enforcing_apfs(kind.to_bytes(), value.f_flags))
}
/// Caller has qualified the complete actual parent chain. This verifies the
/// exact leaf before the unchanged strict open; any later failure is terminal.
#[cfg(target_os = "macos")]
pub fn open_trusted_data_file(parent: &File, name: &Path) -> io::Result<File> {
    use std::os::unix::fs::MetadataExt;
    crate::imp::one_component(name)?;
    if name.as_os_str().as_bytes().contains(&b'/') {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let name = relative(name)?;
    if !ownership_enforcing_data_filesystem(parent)?
        || !crate::private_storage_ancestor_qualified(parent)?
    {
        return Err(refused());
    }
    let mut before = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            before.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let before = unsafe { before.assume_init() };
    if before.st_mode & libc::S_IFMT != libc::S_IFREG
        || before.st_dev as u64 != parent.metadata()?.dev()
    {
        return Err(refused());
    }
    let file = crate::open_file_nofollow(
        parent,
        Path::new(std::ffi::OsStr::from_bytes(name.as_bytes())),
    )?;
    let after = file.metadata()?;
    if !after.is_file()
        || after.dev() != before.st_dev as u64
        || after.ino() != before.st_ino
        || !ownership_enforcing_data_filesystem(&file)?
    {
        return Err(refused());
    }
    Ok(file)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    #[test]
    fn new_mutation_mount_policy_refuses_noowners_even_with_nodev() {
        let local = libc::MNT_LOCAL as u32;
        let noowners = libc::MNT_IGNORE_OWNERSHIP as u32;
        assert!(ownership_enforcing_apfs(b"apfs", local));
        assert!(!ownership_enforcing_apfs(b"apfs", local | noowners));
        assert!(!ownership_enforcing_apfs(
            b"apfs",
            local | noowners | (libc::MNT_NODEV as u32)
        ));
        assert!(!ownership_enforcing_apfs(b"nfs", local));
        assert!(!ownership_enforcing_apfs(b"apfs", 0));
    }
    #[test]
    fn trusted_data_actual_os_home_chain_has_qualified_ownership_boundary() {
        let mut directory = crate::open_absolute_directory_read_nofollow(Path::new("/")).unwrap();
        if let Err(error) = mac_available() {
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
            assert_eq!(
                ownership_enforcing_data_filesystem(&directory)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::Unsupported
            );
            eprintln!("new data namespace: unsupported recorded macOS containment floor");
            return;
        }
        let home = crate::current_user_home().unwrap();
        let mut components = home.components().filter_map(|part| {
            if let Component::Normal(name) = part {
                Some(name)
            } else {
                None
            }
        });
        let mut depth = 0;
        loop {
            let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
            assert_eq!(
                unsafe { libc::fstatfs(directory.as_raw_fd(), fs.as_mut_ptr()) },
                0
            );
            let flags = unsafe { fs.assume_init() }.f_flags;
            let metadata = directory.metadata().unwrap();
            let private = crate::private_storage_ancestor_qualified(&directory).unwrap();
            let ownership = ownership_enforcing_data_filesystem(&directory).unwrap();
            eprintln!("HOME chain index={depth} uid={} mode={:o} flags={flags} private={private} ownership_enforced={ownership} nodev={}",metadata.uid(),metadata.mode()&0o777,flags&(libc::MNT_NODEV as u32)!=0);
            assert!(
                ownership && private,
                "actual HOME admission requires every retained ancestor"
            );
            let Some(name) = components.next() else { break };
            directory = crate::open_directory_read_nofollow(&directory, Path::new(name)).unwrap();
            depth += 1;
        }
    }
    #[test]
    fn trusted_data_leaf_rejects_noncomponent_before_parent_or_external_lookup() {
        let parent = File::open("/").unwrap();
        for bad in ["a/b", "/outside", ".", "..", "a/", ""] {
            assert_eq!(
                open_trusted_data_file(&parent, Path::new(bad))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput,
                "{bad}"
            );
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;
    #[test]
    fn mutation_leaf_qualifies_before_data_open_and_refuses_special_names() {
        use std::io::Read;
        use std::os::unix::fs::symlink;
        let base = crate::tests::TemporaryDirectory::new();
        let root = File::open(&base.0).unwrap();
        let file = base.0.join("owned");
        std::fs::write(&file, b"original").unwrap();
        REGULAR_DATA_OPENS.with(|count| count.set(0));
        let proc = crate::open_absolute_directory_read_nofollow(Path::new("/proc")).unwrap();
        let pin = linux_open(
            &proc,
            &CString::new("version").unwrap(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
        .unwrap();
        assert!(pin.metadata().unwrap().is_file());
        assert_eq!(
            reopen_pinned_regular(&pin).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            REGULAR_DATA_OPENS.with(|count| count.get()),
            0,
            "unsupported actual regular pin must not cause a data open"
        );
        for name in ["a/b", "/outside", ".", "..", "owned/", ""] {
            assert_eq!(
                open_mutation_file_nofollow(&proc, Path::new(name))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
        if let Err(error) = qualify_data_mutation_ancestor(&root) {
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
            assert!(qualified(&root, false).is_err() || mount_id(&root).is_err());
            assert_eq!(REGULAR_DATA_OPENS.with(|count| count.get()), 0);
            assert_eq!(std::fs::read(&file).unwrap(), b"original");
            return;
        }
        let mut opened = open_mutation_file_nofollow(&root, Path::new("owned")).unwrap();
        let mut bytes = Vec::new();
        opened.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"original");
        assert_eq!(REGULAR_DATA_OPENS.with(|count| count.get()), 1);
        std::fs::create_dir(base.0.join("directory")).unwrap();
        symlink(&file, base.0.join("link")).unwrap();
        nix::unistd::mkfifo(
            &base.0.join("fifo"),
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        for name in ["directory", "link", "fifo"] {
            assert!(open_mutation_file_nofollow(&root, Path::new(name)).is_err());
        }
        assert_eq!(
            REGULAR_DATA_OPENS.with(|count| count.get()),
            1,
            "special/link rejection must precede ordinary data acquisition"
        );
        assert_eq!(std::fs::read(file).unwrap(), b"original");
    }
    #[test]
    fn new_mutation_mount_policy_refuses_fuse_and_proc_before_trusting_owners() {
        for kind in [0x65735546, 0x794c7630, 0x9fa0, 0x6969] {
            assert!(!supported_data_class(kind));
        }
        for kind in [0xef53, 0x58465342, 0x9123683e, 0x01021994] {
            assert!(supported_data_class(kind));
        }
        let proc = crate::open_absolute_directory_read_nofollow(Path::new("/proc")).unwrap();
        assert_eq!(
            qualify_data_mutation_ancestor(&proc).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }
    #[test]
    fn pinned_regular_reopen_uses_kernel_self_proc_and_mount_identity() {
        use std::io::Read;
        let base = crate::tests::TemporaryDirectory::new();
        let path = base.0.join("owned");
        std::fs::write(&path, b"owned regular").unwrap();
        let root = File::open(&base.0).unwrap();
        let reopened = open_contained_data_file(&root, Path::new("owned"));
        if let Err(error) = &reopened {
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
            assert!(qualified(&root, true).is_err() || mount_id(&root).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"owned regular");
            return;
        }
        let mut reopened = reopened.unwrap();
        let original = File::open(&path).unwrap();
        assert!(same_data_object(&original, &reopened).unwrap());
        let directory = current_proc_fd_directory().unwrap();
        assert!(mount_id(&directory).unwrap() > 0);
        let mut bytes = Vec::new();
        reopened.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"owned regular");
    }
}
