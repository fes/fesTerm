//! Native-created authority for the fixed HOST receipt child, not a path grant.
use crate::SecretStoreError;
use festerm_unix_security as unix;
use std::{fs::File, io, os::unix::fs::MetadataExt, path::Path};

/// Pinned current-application storage. No path, descriptor, deserializer or
/// caller-supplied owner can construct this capability.
pub struct NativeAppPrivateRoot {
    directory: File,
    identity: (u64, u64),
    policy: RootPolicy,
}

#[derive(Clone, Copy)]
enum RootPolicy {
    #[cfg(any(target_os = "android", test))]
    Private,
    #[cfg(any(target_os = "ios", test))]
    IosSandbox,
}

#[derive(Clone, Copy, Debug)]
enum ApprovalDirectoryStage {
    RootValidate,
    ChildOpen,
    ChildCreate,
    ChildValidate,
    RootSync,
}
impl ApprovalDirectoryStage {
    fn code(self) -> &'static str {
        match self {
            Self::RootValidate => "native.root_validate",
            Self::ChildOpen => "native.child_open",
            Self::ChildCreate => "native.child_create",
            Self::ChildValidate => "native.child_validate",
            Self::RootSync => "native.root_sync",
        }
    }
    fn error(self, error: io::Error) -> io::Error {
        let kind = error.kind();
        let failure = ApprovalDirectoryFailure {
            stage: self,
            kind,
            errno: unix::private_storage_diagnostic(&error)
                .and_then(|(_, _, errno)| errno)
                .or_else(|| error.raw_os_error()),
        };
        // Drop arbitrary source strings. Only this finite diagnostic survives.
        io::Error::new(kind, failure)
    }
}
#[derive(Debug)]
struct ApprovalDirectoryFailure {
    stage: ApprovalDirectoryStage,
    kind: io::ErrorKind,
    errno: Option<i32>,
}
impl std::fmt::Display for ApprovalDirectoryFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("native approval directory operation failed")
    }
}
impl std::error::Error for ApprovalDirectoryFailure {}

impl NativeAppPrivateRoot {
    /// Obtains the current native application root, never imported metadata.
    pub fn current() -> Result<Self, SecretStoreError> {
        #[cfg(target_os = "android")]
        {
            let directory = festerm_android_platform::application_private_directory()
                .map_err(|_| SecretStoreError::LockedOrUnavailable)?;
            Self::pin(directory).map_err(|_| SecretStoreError::LockedOrUnavailable)
        }
        #[cfg(target_os = "ios")]
        {
            ios_application_root().map_err(|_| SecretStoreError::LockedOrUnavailable)
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        {
            Err(SecretStoreError::Unsupported)
        }
    }

    #[cfg(any(target_os = "android", test))]
    fn pin(directory: File) -> io::Result<Self> {
        validate(&directory)?;
        let metadata = directory.metadata()?;
        Ok(Self {
            identity: (metadata.dev(), metadata.ino()),
            directory,
            policy: RootPolicy::Private,
        })
    }

    #[cfg(any(target_os = "ios", test))]
    fn pin_ios(directory: File) -> io::Result<Self> {
        validate_root(&directory, RootPolicy::IosSandbox)?;
        let metadata = directory.metadata()?;
        Ok(Self {
            identity: (metadata.dev(), metadata.ino()),
            directory,
            policy: RootPolicy::IosSandbox,
        })
    }

    fn validate(&self) -> io::Result<()> {
        validate_root(&self.directory, self.policy)?;
        let metadata = self.directory.metadata()?;
        if (metadata.dev(), metadata.ino()) != self.identity {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(())
    }

    /// Opens only the fixed receipt child. The native application root itself
    /// is never exported; all receipt I/O remains descriptor-relative.
    pub fn open_host_approval_directory(&self, create: bool) -> io::Result<File> {
        self.validate()
            .map_err(|error| ApprovalDirectoryStage::RootValidate.error(error))?;
        let name = Path::new("host-client-approvals");
        let child = match unix::open_directory_read_nofollow(&self.directory, name) {
            Ok(child) => child,
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                match unix::create_private_directory(&self.directory, name) {
                    Ok(child) => child,
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        unix::open_directory_read_nofollow(&self.directory, name)
                            .map_err(|error| ApprovalDirectoryStage::ChildOpen.error(error))?
                    }
                    Err(error) => return Err(ApprovalDirectoryStage::ChildCreate.error(error)),
                }
            }
            Err(error) => return Err(ApprovalDirectoryStage::ChildOpen.error(error)),
        };
        validate(&child).map_err(|error| ApprovalDirectoryStage::ChildValidate.error(error))?;
        if create {
            self.directory
                .sync_all()
                .map_err(|error| ApprovalDirectoryStage::RootSync.error(error))?;
        }
        Ok(child)
    }

    /// Finite context only: never a path, descriptor, ACL, record or source text.
    /// A lower security helper may already have discarded the OS errno.
    pub fn approval_directory_diagnostic(
        error: &io::Error,
    ) -> Option<(&'static str, io::ErrorKind, Option<i32>)> {
        let failure = error
            .get_ref()?
            .downcast_ref::<ApprovalDirectoryFailure>()?;
        Some((failure.stage.code(), failure.kind, failure.errno))
    }

    /// Checks that the existing profile store is this OS-issued namespace.
    /// A matching caller path grants no authority and never creates anything.
    #[cfg(any(target_os = "ios", test))]
    pub fn matches_profile_directory(&self, path: &Path) -> io::Result<bool> {
        self.validate()?;
        let directory = open_ios_selected_directory(path)?;
        let metadata = directory.metadata()?;
        Ok((metadata.dev(), metadata.ino()) == self.identity)
    }

    /// Compares pinned native namespace identity, never an ordinary path.
    pub fn same_root(&self, other: &Self) -> bool {
        self.identity == other.identity && self.validate().is_ok() && other.validate().is_ok()
    }
}

// Root authority comes from native app selection, never a caller path. The
// receipt child below remains strict0700 on every platform.
fn validate_root(directory: &File, policy: RootPolicy) -> io::Result<()> {
    #[cfg(not(any(target_os = "android", target_os = "ios", test)))]
    let _ = directory;
    match policy {
        #[cfg(any(target_os = "android", test))]
        RootPolicy::Private => validate(directory),
        #[cfg(any(target_os = "ios", test))]
        RootPolicy::IosSandbox => {
            let metadata = directory.metadata()?;
            if !metadata.is_dir()
                || metadata.nlink() == 0
                || !ios_root_owner_and_mode(
                    metadata.uid(),
                    nix::unistd::Uid::effective().as_raw(),
                    metadata.mode(),
                )
            {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            unix::secure_staging_parent(directory).map(|_| ())
        }
    }
}

#[cfg(any(target_os = "ios", test))]
fn ios_root_owner_and_mode(uid: u32, current_uid: u32, mode: u32) -> bool {
    uid == current_uid && mode & 0o022 == 0
}

#[cfg(any(target_os = "ios", test))]
fn open_ios_application_root(application_support: &Path) -> io::Result<NativeAppPrivateRoot> {
    let directory = open_ios_selected_directory(application_support)?;
    validate_root(&directory, RootPolicy::IosSandbox)?;
    let profile = unix::open_directory_read_nofollow(&directory, Path::new("festerm"))?;
    NativeAppPrivateRoot::pin_ios(profile)
}

#[cfg(any(target_os = "ios", test))]
fn ios_var_alias_path(path: &Path, target: &Path) -> io::Result<std::path::PathBuf> {
    if !matches!(target.to_str(), Some("private/var" | "/private/var")) {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(Path::new("/private/var").join(
        path.strip_prefix("/var")
            .map_err(|_| io::ErrorKind::PermissionDenied)?,
    ))
}

#[cfg(any(target_os = "ios", test))]
fn ios_selected_path(path: &Path) -> io::Result<std::path::PathBuf> {
    if !path.is_absolute() {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    if path.starts_with("/var") {
        let metadata = std::fs::symlink_metadata("/var")?;
        if metadata.is_symlink() {
            return ios_var_alias_path(path, &std::fs::read_link("/var")?);
        }
        if !metadata.is_dir() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
    }
    Ok(path.to_path_buf())
}

#[cfg(any(target_os = "ios", test))]
fn open_ios_selected_directory(path: &Path) -> io::Result<File> {
    unix::open_absolute_directory_read_nofollow(&ios_selected_path(path)?)
}

#[cfg(target_os = "ios")]
fn ios_application_root() -> io::Result<NativeAppPrivateRoot> {
    use objc2_foundation::{NSFileManager, NSSearchPathDirectory, NSSearchPathDomainMask};

    let path = objc2::rc::autoreleasepool(|_| {
        let urls = NSFileManager::defaultManager().URLsForDirectory_inDomains(
            NSSearchPathDirectory::ApplicationSupportDirectory,
            NSSearchPathDomainMask::UserDomainMask,
        );
        if urls.count() != 1 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let url = urls.firstObject().ok_or(io::ErrorKind::NotFound)?;
        if !url.isFileURL() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok::<_, io::Error>(url.path().ok_or(io::ErrorKind::NotFound)?.to_string())
    })?;
    open_ios_application_root(Path::new(&path))
}

fn validate(directory: &File) -> io::Result<()> {
    if directory.metadata()?.nlink() == 0 || !unix::is_current_user_only(directory, true)? {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::current_dir()
                .unwrap()
                .join("target/native-root-tests")
                .join(uuid::Uuid::new_v4().to_string());
            fs::create_dir_all(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
        fn pin(&self) -> NativeAppPrivateRoot {
            NativeAppPrivateRoot::pin(File::open(&self.0).unwrap()).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn native_root_approval_diagnostic_is_finite_preserves_kind_and_known_errno() {
        for stage in [
            ApprovalDirectoryStage::RootValidate,
            ApprovalDirectoryStage::ChildOpen,
            ApprovalDirectoryStage::ChildCreate,
            ApprovalDirectoryStage::ChildValidate,
            ApprovalDirectoryStage::RootSync,
        ] {
            let error = stage.error(io::Error::from_raw_os_error(nix::libc::EACCES));
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(
                NativeAppPrivateRoot::approval_directory_diagnostic(&error),
                Some((
                    stage.code(),
                    io::ErrorKind::PermissionDenied,
                    Some(nix::libc::EACCES)
                ))
            );
        }
        let error = ApprovalDirectoryStage::ChildCreate.error(io::Error::new(
            io::ErrorKind::Unsupported,
            "secret-reference /private/key ghp_sensitive",
        ));
        assert_eq!(
            NativeAppPrivateRoot::approval_directory_diagnostic(&error),
            Some(("native.child_create", io::ErrorKind::Unsupported, None))
        );
        assert_eq!(
            error.to_string(),
            "native approval directory operation failed"
        );
        if let Some(source) = std::error::Error::source(&error) {
            assert_eq!(
                source.to_string(),
                "native approval directory operation failed"
            );
        }
        assert!(
            NativeAppPrivateRoot::approval_directory_diagnostic(&io::Error::other("unrelated"))
                .is_none()
        );
    }

    #[test]
    fn native_root_approval_diagnostic_distinguishes_child_open_and_validation_without_repair() {
        let fixture = Fixture::new();
        let root = fixture.pin();
        let child = fixture.0.join("host-client-approvals");
        fs::write(&child, b"preserve existing object").unwrap();
        let error = root.open_host_approval_directory(true).unwrap_err();
        assert_eq!(
            NativeAppPrivateRoot::approval_directory_diagnostic(&error)
                .unwrap()
                .0,
            "native.child_open"
        );
        assert_eq!(fs::read(&child).unwrap(), b"preserve existing object");
        // Only this owned fixture changes shape; the product never repairs it.
        fs::remove_file(&child).unwrap();
        fs::create_dir(&child).unwrap();
        fs::set_permissions(&child, fs::Permissions::from_mode(0o755)).unwrap();
        let error = root.open_host_approval_directory(true).unwrap_err();
        assert_eq!(
            NativeAppPrivateRoot::approval_directory_diagnostic(&error)
                .unwrap()
                .0,
            "native.child_validate"
        );
        assert_eq!(fs::metadata(&child).unwrap().mode() & 0o777, 0o755);
    }

    #[test]
    fn ios_native_root_retains_existing_namespace_below_os_managed_ancestor() {
        let fixture = Fixture::new();
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o775)).unwrap();
        let support = fixture.0.join("Application Support");
        let profile = support.join("festerm");
        fs::create_dir_all(&profile).unwrap();
        fs::set_permissions(&support, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&profile, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(NativeAppPrivateRoot::pin(File::open(&profile).unwrap()).is_err());
        let root = open_ios_application_root(&support).unwrap();
        assert!(root.matches_profile_directory(&profile).unwrap());
        assert!(!root.matches_profile_directory(&support).unwrap());
        assert!(!root.matches_profile_directory(&Fixture::new().0).unwrap());
        root.open_host_approval_directory(true).unwrap();
        let child = profile.join("host-client-approvals");
        assert_eq!(fs::metadata(&child).unwrap().mode() & 0o777, 0o700);
        fs::write(child.join("retained"), b"owned fixture record").unwrap();
        assert!(root.same_root(&open_ios_application_root(&support).unwrap()));
        assert_eq!(
            fs::read(child.join("retained")).unwrap(),
            b"owned fixture record"
        );
        assert_eq!(fs::metadata(&fixture.0).unwrap().mode() & 0o777, 0o775);
        assert_eq!(fs::metadata(&profile).unwrap().mode() & 0o777, 0o755);
    }

    #[test]
    fn ios_native_root_traverses_search_only_ancestor_without_reading_it() {
        let fixture = Fixture::new();
        let support = fixture.0.join("Application Support");
        let profile = support.join("festerm");
        fs::create_dir_all(&profile).unwrap();
        fs::set_permissions(&support, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&profile, fs::Permissions::from_mode(0o755)).unwrap();
        // Only this fresh owned fixture changes mode. The selected namespace
        // remains readable; an OS-managed ancestor need only permit search.
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o111)).unwrap();
        let ancestor_read = File::open(&fixture.0);
        let opened = open_ios_application_root(&support);
        let matched = opened
            .as_ref()
            .map(|root| root.matches_profile_directory(&profile));
        let child = opened
            .as_ref()
            .map(|root| root.open_host_approval_directory(true));
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            ancestor_read.unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(
            opened.is_ok(),
            "search-only ancestor must not require read access"
        );
        assert!(matched.unwrap().unwrap());
        assert_eq!(
            child.unwrap().unwrap().metadata().unwrap().mode() & 0o777,
            0o700
        );
        assert_eq!(fs::metadata(&support).unwrap().mode() & 0o777, 0o755);
        assert_eq!(fs::metadata(&profile).unwrap().mode() & 0o777, 0o755);
    }

    #[test]
    fn ios_native_root_still_requires_search_and_selected_directory_read_access() {
        for (ancestor_mode, selected_mode) in [(0o000, 0o755), (0o700, 0o111)] {
            let fixture = Fixture::new();
            let support = fixture.0.join("Application Support");
            let profile = support.join("festerm");
            fs::create_dir_all(&profile).unwrap();
            fs::set_permissions(&support, fs::Permissions::from_mode(0o755)).unwrap();
            fs::set_permissions(&profile, fs::Permissions::from_mode(selected_mode)).unwrap();
            fs::set_permissions(&fixture.0, fs::Permissions::from_mode(ancestor_mode)).unwrap();
            let opened = open_ios_selected_directory(&profile);
            fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o700)).unwrap();
            fs::set_permissions(&profile, fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(opened.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        }
    }

    #[test]
    fn ios_native_root_refuses_symlinks_permissive_children_and_revoked_roots() {
        let fixture = Fixture::new();
        let support = fixture.0.join("Application Support");
        let profile = support.join("festerm");
        fs::create_dir_all(&profile).unwrap();
        fs::set_permissions(&support, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&profile, fs::Permissions::from_mode(0o755)).unwrap();
        let root = open_ios_application_root(&support).unwrap();
        let child = profile.join("host-client-approvals");
        fs::create_dir(&child).unwrap();
        fs::set_permissions(&child, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(root.open_host_approval_directory(true).is_err());
        assert_eq!(fs::metadata(&child).unwrap().mode() & 0o777, 0o755);
        fs::remove_dir(&child).unwrap();
        let other = Fixture::new();
        std::os::unix::fs::symlink(&other.0, &child).unwrap();
        assert!(root.open_host_approval_directory(true).is_err());
        fs::remove_file(&child).unwrap();
        fs::set_permissions(&profile, fs::Permissions::from_mode(0o775)).unwrap();
        assert!(root.open_host_approval_directory(true).is_err());
        fs::set_permissions(&profile, fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_dir(&profile).unwrap();
        assert!(root.open_host_approval_directory(true).is_err());
        std::os::unix::fs::symlink(&other.0, &profile).unwrap();
        assert!(open_ios_application_root(&support).is_err());
        fs::remove_file(&profile).unwrap();
        fs::create_dir(&profile).unwrap();
        assert!(!root.same_root(&open_ios_application_root(&support).unwrap()));
        assert!(!ios_root_owner_and_mode(502, 501, 0o755));
        assert!(!ios_root_owner_and_mode(0, 501, 0o755));
        assert!(!ios_root_owner_and_mode(501, 501, 0o775));
        assert!(open_ios_application_root(Path::new("relative")).is_err());
    }

    #[test]
    fn ios_native_root_only_normalizes_the_documented_var_alias() {
        let selected = Path::new(
            "/var/mobile/Containers/Data/Application/fixture/Library/Application Support",
        );
        let expected = Path::new(
            "/private/var/mobile/Containers/Data/Application/fixture/Library/Application Support",
        );
        for target in [Path::new("private/var"), Path::new("/private/var")] {
            assert_eq!(ios_var_alias_path(selected, target).unwrap(), expected);
        }
        for target in [
            "/tmp",
            "../private/var",
            "private/var/other",
            "/private/var/../var",
        ] {
            assert!(ios_var_alias_path(selected, Path::new(target)).is_err());
        }
        assert!(
            ios_var_alias_path(Path::new("/variable/fixture"), Path::new("private/var")).is_err()
        );
        assert_eq!(
            ios_selected_path(Path::new("/Users/public-fixture")).unwrap(),
            Path::new("/Users/public-fixture")
        );
        // Read-only check of the host OS alias, no changes to /var or its target.
        let var = fs::symlink_metadata("/var").unwrap();
        let normalized = ios_selected_path(Path::new("/var")).unwrap();
        if var.is_symlink() {
            assert_eq!(normalized, Path::new("/private/var"));
        } else {
            assert!(var.is_dir());
            assert_eq!(normalized, Path::new("/var"));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ios_native_root_refuses_actual_acl_grants_without_repair() {
        let fixture = Fixture::new();
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o755)).unwrap();
        let result = std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone allow add_file"])
            .arg(&fixture.0)
            .status()
            .unwrap();
        assert!(result.success());
        assert!(NativeAppPrivateRoot::pin_ios(File::open(&fixture.0).unwrap()).is_err());
        assert_eq!(fs::metadata(&fixture.0).unwrap().mode() & 0o777, 0o755);
    }

    #[test]
    fn native_root_current_owner_and_fixed_child_reopen_without_reset() {
        let fixture = Fixture::new();
        let root = fixture.pin();
        assert_eq!(
            root.open_host_approval_directory(false).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        root.open_host_approval_directory(true).unwrap();
        let path = fixture.0.join("host-client-approvals/issued");
        fs::write(&path, b"retained original").unwrap();
        fixture.pin().open_host_approval_directory(true).unwrap();
        assert_eq!(fs::read(path).unwrap(), b"retained original");
        assert!(root.same_root(&fixture.pin()));
        assert!(!root.same_root(&Fixture::new().pin()));
    }

    #[test]
    fn native_root_rejects_nonprivate_root_child_and_symlink_without_repair() {
        let fixture = Fixture::new();
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o750)).unwrap();
        assert!(NativeAppPrivateRoot::pin(File::open(&fixture.0).unwrap()).is_err());
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o700)).unwrap();
        let root = fixture.pin();
        let child = fixture.0.join("host-client-approvals");
        fs::create_dir(&child).unwrap();
        fs::set_permissions(&child, fs::Permissions::from_mode(0o750)).unwrap();
        assert!(root.open_host_approval_directory(true).is_err());
        fs::remove_dir(&child).unwrap();
        let other = Fixture::new();
        std::os::unix::fs::symlink(&other.0, &child).unwrap();
        assert!(root.open_host_approval_directory(true).is_err());
        assert!(fs::symlink_metadata(child).unwrap().is_symlink());
    }

    #[test]
    fn native_root_revoked_permissions_and_unlinked_root_fail_closed() {
        let fixture = Fixture::new();
        let root = fixture.pin();
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o770)).unwrap();
        assert!(root.open_host_approval_directory(true).is_err());
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir(&fixture.0).unwrap();
        assert!(root.open_host_approval_directory(true).is_err());
        fs::create_dir(&fixture.0).unwrap();
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!root.same_root(&fixture.pin()));
    }
}
