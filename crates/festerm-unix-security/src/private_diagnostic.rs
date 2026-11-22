//! Finite private-storage failures. No source text enters diagnostic context;
//! unmapped errors pass through unchanged.
use std::io;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Operation {
    #[cfg(any(target_os = "macos", target_os = "ios", test))]
    AclInit,
    #[cfg(any(target_os = "macos", target_os = "ios", test))]
    AclSet,
    #[cfg(any(target_os = "macos", target_os = "ios", test))]
    AclFree,
    #[cfg(any(target_os = "linux", target_os = "android", test))]
    AttributeRead,
    #[cfg(any(target_os = "linux", target_os = "android", test))]
    AttributeRemove,
    Chmod,
    MetadataRead,
    ModePostcondition,
    AclPostcondition,
    FileSync,
    DirectorySync,
    ExclusiveRename,
}
impl Operation {
    fn code(self) -> &'static str {
        match self {
            #[cfg(any(target_os = "macos", target_os = "ios", test))]
            Self::AclInit => "private.acl_init",
            #[cfg(any(target_os = "macos", target_os = "ios", test))]
            Self::AclSet => "private.acl_set",
            #[cfg(any(target_os = "macos", target_os = "ios", test))]
            Self::AclFree => "private.acl_free",
            #[cfg(any(target_os = "linux", target_os = "android", test))]
            Self::AttributeRead => "private.attribute_read",
            #[cfg(any(target_os = "linux", target_os = "android", test))]
            Self::AttributeRemove => "private.attribute_remove",
            Self::Chmod => "private.chmod",
            Self::MetadataRead => "private.metadata_read",
            Self::ModePostcondition => "private.mode_postcondition",
            Self::AclPostcondition => "private.acl_postcondition",
            Self::FileSync => "private.file_sync",
            Self::DirectorySync => "private.directory_sync",
            Self::ExclusiveRename => "private.exclusive_rename",
        }
    }
}
#[derive(Debug)]
struct Failure {
    operation: Operation,
    kind: io::ErrorKind,
    errno: Option<i32>,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("private storage operation failed")
    }
}
impl std::error::Error for Failure {}

/// Only fixed operation codes, the unchanged error kind and known numeric errno.
pub fn private_storage_diagnostic(
    error: &io::Error,
) -> Option<(&'static str, io::ErrorKind, Option<i32>)> {
    let value = error.get_ref()?.downcast_ref::<Failure>()?;
    Some((value.operation.code(), value.kind, value.errno))
}
fn failure(operation: Operation, kind: io::ErrorKind, errno: Option<i32>) -> io::Error {
    io::Error::new(
        kind,
        Failure {
            operation,
            kind,
            errno,
        },
    )
}
pub(crate) fn private_error(operation: Operation, error: io::Error) -> io::Error {
    if private_storage_diagnostic(&error).is_some() {
        return error;
    }
    match error.raw_os_error() {
        Some(
            code @ (nix::libc::EPERM | nix::libc::EACCES | nix::libc::EINVAL | nix::libc::ENOTSUP),
        ) => failure(operation, io::ErrorKind::Unsupported, Some(code)),
        _ => error,
    }
}
pub(crate) fn private_postcondition(operation: Operation) -> io::Error {
    failure(operation, io::ErrorKind::Unsupported, None)
}
pub(crate) fn exclusive_rename_error(error: nix::errno::Errno) -> io::Error {
    if [
        nix::errno::Errno::EINVAL,
        nix::errno::Errno::ENOSYS,
        nix::errno::Errno::ENOTSUP,
    ]
    .contains(&error)
    {
        failure(
            Operation::ExclusiveRename,
            io::ErrorKind::Unsupported,
            Some(error as i32),
        )
    } else {
        io::Error::from(error)
    }
}

// Capture each failure before the following cleanup can change thread errno.
#[cfg(any(target_os = "macos", target_os = "ios", test))]
pub(crate) fn update_acl_and_free(
    set: impl FnOnce() -> i32,
    free: impl FnOnce() -> i32,
    mut last_error: impl FnMut() -> io::Error,
) -> io::Result<()> {
    let set_error = (set() != 0).then(|| private_error(Operation::AclSet, last_error()));
    let free_error = (free() != 0).then(|| private_error(Operation::AclFree, last_error()));
    match set_error.or(free_error) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_storage_context_preserves_sanitized_errno_and_leaves_unmapped_errors_unchanged() {
        for operation in [
            Operation::AclInit,
            Operation::AclSet,
            Operation::AclFree,
            Operation::AttributeRead,
            Operation::AttributeRemove,
            Operation::Chmod,
            Operation::MetadataRead,
            Operation::FileSync,
            Operation::DirectorySync,
        ] {
            for code in [
                nix::libc::EPERM,
                nix::libc::EACCES,
                nix::libc::EINVAL,
                nix::libc::ENOTSUP,
            ] {
                let error = private_error(operation, io::Error::from_raw_os_error(code));
                assert_eq!(error.kind(), io::ErrorKind::Unsupported);
                assert_eq!(error.raw_os_error(), None);
                assert_eq!(
                    private_storage_diagnostic(&error),
                    Some((operation.code(), io::ErrorKind::Unsupported, Some(code)))
                );
            }
            let error = private_error(
                operation,
                io::Error::other("/private/seed ghp_secret acl=x reference=opaque"),
            );
            assert_eq!(error.kind(), io::ErrorKind::Other);
            assert_eq!(
                error.to_string(),
                "/private/seed ghp_secret acl=x reference=opaque"
            );
            assert!(private_storage_diagnostic(&error).is_none());
            let error = private_error(operation, io::Error::from_raw_os_error(nix::libc::EIO));
            assert_eq!(error.raw_os_error(), Some(nix::libc::EIO));
            assert!(private_storage_diagnostic(&error).is_none());
        }
        for (operation, code) in [
            (Operation::ModePostcondition, "private.mode_postcondition"),
            (Operation::AclPostcondition, "private.acl_postcondition"),
        ] {
            let error = private_postcondition(operation);
            assert_eq!(error.raw_os_error(), None);
            assert_eq!(
                private_storage_diagnostic(&error),
                Some((code, io::ErrorKind::Unsupported, None))
            );
        }
        for code in [
            nix::errno::Errno::EINVAL,
            nix::errno::Errno::ENOSYS,
            nix::errno::Errno::ENOTSUP,
        ] {
            let error = exclusive_rename_error(code);
            assert_eq!(error.raw_os_error(), None);
            assert_eq!(
                private_storage_diagnostic(&error),
                Some((
                    "private.exclusive_rename",
                    io::ErrorKind::Unsupported,
                    Some(code as i32)
                ))
            );
        }
        let exists = exclusive_rename_error(nix::errno::Errno::EEXIST);
        assert_eq!(exists.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(exists.raw_os_error(), Some(nix::libc::EEXIST));
        let io = exclusive_rename_error(nix::errno::Errno::EIO);
        assert_eq!(io.raw_os_error(), Some(nix::libc::EIO));
    }
    #[test]
    fn acl_set_errno_is_captured_before_free_and_primary_failure_survives_cleanup_failure() {
        use std::cell::Cell;
        for cleanup_fails in [false, true] {
            let errno = Cell::new(0);
            let error = update_acl_and_free(
                || {
                    errno.set(nix::libc::EPERM);
                    -1
                },
                || {
                    errno.set(nix::libc::EINVAL);
                    if cleanup_fails {
                        -1
                    } else {
                        0
                    }
                },
                || io::Error::from_raw_os_error(errno.get()),
            )
            .unwrap_err();
            assert_eq!(
                private_storage_diagnostic(&error),
                Some((
                    "private.acl_set",
                    io::ErrorKind::Unsupported,
                    Some(nix::libc::EPERM)
                ))
            );
            let outer = private_error(Operation::MetadataRead, error);
            assert_eq!(
                private_storage_diagnostic(&outer).unwrap().0,
                "private.acl_set"
            );
        }
        let error = update_acl_and_free(
            || 0,
            || -1,
            || io::Error::from_raw_os_error(nix::libc::EACCES),
        )
        .unwrap_err();
        assert_eq!(
            private_storage_diagnostic(&error),
            Some((
                "private.acl_free",
                io::ErrorKind::Unsupported,
                Some(nix::libc::EACCES)
            ))
        );
    }
}
