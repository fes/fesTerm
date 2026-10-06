//! Safe ownership around Win32 object security and local named-pipe operations.

#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(windows)]
pub mod named_pipe;

#[cfg(windows)]
mod imp {
    use std::{
        ffi::OsString,
        fs::File,
        io, mem,
        os::windows::{
            ffi::{OsStrExt, OsStringExt},
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        },
        path::{Path, PathBuf},
        ptr,
    };

    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, GetLastError, SetHandleInformation, GENERIC_ALL, GENERIC_READ,
            GENERIC_WRITE, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
        },
        Security::{
            AddAccessAllowedAceEx, EqualSid, GetLengthSid, GetTokenInformation, InitializeAcl,
            InitializeSecurityDescriptor, SetSecurityDescriptorControl, SetSecurityDescriptorDacl,
            SetTokenInformation, TokenDefaultDacl, TokenUser, ACL, ACL_REVISION,
            SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SE_DACL_PROTECTED, TOKEN_ADJUST_DEFAULT,
            TOKEN_DEFAULT_DACL, TOKEN_QUERY, TOKEN_USER,
        },
        Storage::FileSystem::{
            CreateFileW, GetFinalPathNameByHandleW, ReplaceFileW, CREATE_NEW, FILE_ALL_ACCESS,
            FILE_ATTRIBUTE_NORMAL, FILE_NAME_NORMALIZED, VOLUME_NAME_DOS,
        },
        System::{
            Console::{
                GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
            },
            Threading::{
                GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
    };

    /// Restores the process token's original default DACL and closes the token.
    pub struct DefaultDaclGuard {
        token: HANDLE,
        original: Vec<usize>,
        restored: bool,
    }

    impl DefaultDaclGuard {
        /// Restores the token before launching any child process that could
        /// otherwise inherit the temporary current-user-only default DACL.
        pub fn restore(mut self) -> io::Result<()> {
            let result = self.restore_inner();
            self.restored = result.is_ok();
            result
        }

        fn restore_inner(&mut self) -> io::Result<()> {
            set_token_default_dacl(self.token, self.original.as_ptr().cast())
        }
    }

    impl Drop for DefaultDaclGuard {
        fn drop(&mut self) {
            if !self.restored {
                let _ = self.restore_inner();
            }
            let _ = unsafe { CloseHandle(self.token) };
        }
    }

    /// Temporarily changes this process token's default DACL to one ACE that
    /// grants the current user full access.
    ///
    /// Win32's named-pipe API uses the token default DACL when passed null
    /// security attributes. Callers create the pipe while this guard is live,
    /// then call [`DefaultDaclGuard::restore`] before launching the shell.
    pub fn restrict_default_dacl_to_current_user() -> io::Result<DefaultDaclGuard> {
        let mut token = ptr::null_mut();
        let opened = unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY | TOKEN_ADJUST_DEFAULT,
                &raw mut token,
            )
        };
        if opened == 0 {
            return Err(io::Error::last_os_error());
        }

        let result = (|| {
            let original = token_information(token, TokenDefaultDacl)?;
            let user = token_information(token, TokenUser)?;
            let token_user = unsafe { &*user.as_ptr().cast::<TOKEN_USER>() };
            let sid_length = unsafe { GetLengthSid(token_user.User.Sid) };
            if sid_length == 0 {
                return Err(io::Error::last_os_error());
            }

            let acl_bytes = mem::size_of::<ACL>()
                + mem::size_of::<windows_sys::Win32::Security::ACCESS_ALLOWED_ACE>()
                - mem::size_of::<u32>()
                + sid_length as usize;
            let mut acl_storage = vec![0usize; acl_bytes.div_ceil(mem::size_of::<usize>())];
            let acl = acl_storage.as_mut_ptr().cast::<ACL>();
            let initialized = unsafe { InitializeAcl(acl, acl_bytes as u32, ACL_REVISION) };
            if initialized == 0 {
                return Err(io::Error::last_os_error());
            }
            let added = unsafe {
                AddAccessAllowedAceEx(acl, ACL_REVISION, 0, GENERIC_ALL, token_user.User.Sid)
            };
            if added == 0 {
                return Err(io::Error::last_os_error());
            }

            let restricted = TOKEN_DEFAULT_DACL { DefaultDacl: acl };
            set_token_default_dacl(token, (&raw const restricted).cast())?;
            Ok(DefaultDaclGuard {
                token,
                original,
                restored: false,
            })
        })();

        if result.is_err() {
            let _ = unsafe { CloseHandle(token) };
        }
        result
    }

    /// Creates a new file beneath `directory` with a protected current-user-only
    /// DACL, before any caller bytes can be written. Because Win32 does not
    /// accept a directory handle as `CreateFileW`'s root, the caller must
    /// re-open `name` through its retained directory capability and compare
    /// file identity before writing.
    pub fn create_current_user_only_file(directory: &File, name: &Path) -> io::Result<File> {
        if name.components().count() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the private file name must be one path component",
            ));
        }
        let mut token = ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let user = token_information(token.as_raw_handle(), TokenUser)?;
        let token_user = unsafe { &*user.as_ptr().cast::<TOKEN_USER>() };
        let sid_length = unsafe { GetLengthSid(token_user.User.Sid) };
        if sid_length == 0 {
            return Err(io::Error::last_os_error());
        }

        let acl_bytes = mem::size_of::<ACL>()
            + mem::size_of::<windows_sys::Win32::Security::ACCESS_ALLOWED_ACE>()
            - mem::size_of::<u32>()
            + sid_length as usize;
        let mut acl_storage = vec![0usize; acl_bytes.div_ceil(mem::size_of::<usize>())];
        let acl = acl_storage.as_mut_ptr().cast::<ACL>();
        if unsafe { InitializeAcl(acl, acl_bytes as u32, ACL_REVISION) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe {
            AddAccessAllowedAceEx(acl, ACL_REVISION, 0, FILE_ALL_ACCESS, token_user.User.Sid)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }

        let mut descriptor = SECURITY_DESCRIPTOR::default();
        if unsafe { InitializeSecurityDescriptor((&raw mut descriptor).cast(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { SetSecurityDescriptorDacl((&raw mut descriptor).cast(), 1, acl, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe {
            SetSecurityDescriptorControl(
                (&raw mut descriptor).cast(),
                SE_DACL_PROTECTED,
                SE_DACL_PROTECTED,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: (&raw mut descriptor).cast(),
            bInheritHandle: 0,
        };
        let path = directory_child_path(directory, name)?;
        let wide = wide_path(&path);
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                &raw const attributes,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { File::from_raw_handle(handle) })
        }
    }

    /// Atomically replaces `target` with `replacement`, preserving the target
    /// file's Windows attributes and ACL. The caller must verify the resulting
    /// target's identity through its retained directory capability before
    /// reporting success.
    pub fn replace_file_preserving_security(
        directory: &File,
        replacement: &Path,
        target: &Path,
    ) -> io::Result<()> {
        let replacement = wide_path(&directory_child_path(directory, replacement)?);
        let target = wide_path(&directory_child_path(directory, target)?);
        let replaced = unsafe {
            ReplaceFileW(
                target.as_ptr(),
                replacement.as_ptr(),
                ptr::null(),
                0,
                ptr::null(),
                ptr::null(),
            )
        };
        if replaced == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn directory_child_path(directory: &File, name: &Path) -> io::Result<PathBuf> {
        if name.components().count() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the file name must be one path component",
            ));
        }
        let handle = directory.as_raw_handle();
        let required = unsafe {
            GetFinalPathNameByHandleW(
                handle,
                ptr::null_mut(),
                0,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        };
        if required == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0u16; required as usize + 1];
        let written = unsafe {
            GetFinalPathNameByHandleW(
                handle,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        };
        if written == 0 || written as usize >= buffer.len() {
            return Err(io::Error::last_os_error());
        }
        buffer.truncate(written as usize);
        Ok(PathBuf::from(OsString::from_wide(&buffer)).join(name))
    }

    fn wide_path(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    fn token_information(token: HANDLE, information_class: i32) -> io::Result<Vec<usize>> {
        let mut required = 0;
        let _ = unsafe {
            GetTokenInformation(
                token,
                information_class,
                ptr::null_mut(),
                0,
                &raw mut required,
            )
        };
        if required == 0 {
            return Err(io::Error::from_raw_os_error(unsafe {
                GetLastError() as i32
            }));
        }
        let mut storage = vec![0usize; (required as usize).div_ceil(mem::size_of::<usize>())];
        let loaded = unsafe {
            GetTokenInformation(
                token,
                information_class,
                storage.as_mut_ptr().cast(),
                required,
                &raw mut required,
            )
        };
        if loaded == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(storage)
        }
    }

    pub(super) fn process_belongs_to_current_user(process_id: u32) -> io::Result<bool> {
        // SAFETY: querying a PID does not transfer or inherit any caller handle.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: OpenProcess returned one owned valid handle.
        let process = unsafe { OwnedHandle::from_raw_handle(process) };
        let server_user = process_user(process.as_raw_handle())?;
        // SAFETY: this pseudo-handle is borrowed, never closed.
        let own_user = process_user(unsafe { GetCurrentProcess() })?;
        // SAFETY: both aligned buffers contain TOKEN_USER records returned by
        // GetTokenInformation and remain alive throughout the SID comparison.
        let same_user = unsafe {
            let server = &*server_user.as_ptr().cast::<TOKEN_USER>();
            let own = &*own_user.as_ptr().cast::<TOKEN_USER>();
            EqualSid(server.User.Sid, own.User.Sid) != 0
        };
        Ok(same_user)
    }

    fn process_user(process: HANDLE) -> io::Result<Vec<usize>> {
        let mut token = ptr::null_mut();
        // SAFETY: process is a live borrowed process handle and token is writable.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: OpenProcessToken returned one owned valid handle.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        token_information(token.as_raw_handle(), TokenUser)
    }

    fn set_token_default_dacl(
        token: HANDLE,
        information: *const core::ffi::c_void,
    ) -> io::Result<()> {
        let updated = unsafe {
            SetTokenInformation(
                token,
                TokenDefaultDacl,
                information,
                mem::size_of::<TOKEN_DEFAULT_DACL>() as u32,
            )
        };
        if updated == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Clears `HANDLE_FLAG_INHERIT` on this process's own stdin/stdout/stderr
    /// handles, if any are set.
    ///
    /// Spawning a child process that redirects its own stdio (even to NUL)
    /// forces Windows to create it with `bInheritHandles = TRUE`, which
    /// duplicates *every* inheritable handle in this process into the
    /// child — not just the three explicitly redirected ones. If this
    /// process's own stdout or stderr was itself piped by its caller (as
    /// `festerm-sessiond start`'s is by fesTerm, which reads the pipe via
    /// `Command::output()`), that pipe's write end is inheritable by
    /// default. A long-lived, deliberately detached grandchild (fesTerm's
    /// persistence daemon) would otherwise inherit a duplicate write
    /// handle to it; since the daemon never exits, that duplicate handle
    /// would keep the pipe open forever and hang the caller's blocking
    /// read. Call this immediately before spawning such a child.
    pub fn disable_std_handle_inheritance() {
        const STD_HANDLES: [STD_HANDLE; 3] =
            [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];
        for which in STD_HANDLES {
            let handle = unsafe { GetStdHandle(which) };
            if handle.is_null() || handle as isize == -1 {
                continue;
            }
            let _ = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{
            fs::{self, OpenOptions},
            io::Write,
            os::windows::fs::OpenOptionsExt,
            time::{SystemTime, UNIX_EPOCH},
        };
        use windows_sys::Win32::{
            Foundation::{LocalFree, ERROR_SUCCESS},
            Security::{
                AclSizeInformation,
                Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
                GetAce, GetAclInformation, GetSecurityDescriptorControl, ACCESS_ALLOWED_ACE,
                ACL_SIZE_INFORMATION, DACL_SECURITY_INFORMATION, SE_DACL_PROTECTED,
            },
            Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS,
        };

        struct TemporaryDirectory(PathBuf);

        impl TemporaryDirectory {
            fn new() -> Self {
                let nonce = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let path = std::env::temp_dir().join(format!(
                    "festerm-windows-security-{}-{nonce}",
                    std::process::id()
                ));
                fs::create_dir(&path).unwrap();
                Self(path)
            }

            fn handle(&self) -> File {
                OpenOptions::new()
                    .read(true)
                    .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                    .open(&self.0)
                    .unwrap()
            }
        }

        impl Drop for TemporaryDirectory {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        #[test]
        fn private_creation_and_replacement_keep_the_current_user_only_dacl() {
            let directory = TemporaryDirectory::new();
            let directory_handle = directory.handle();
            let mut target =
                create_current_user_only_file(&directory_handle, Path::new("target.md")).unwrap();
            target.write_all(b"before").unwrap();
            target.sync_all().unwrap();
            assert_current_user_only_dacl(&target);
            drop(target);

            fs::write(directory.0.join("replacement.tmp"), b"after").unwrap();
            replace_file_preserving_security(
                &directory_handle,
                Path::new("replacement.tmp"),
                Path::new("target.md"),
            )
            .unwrap();

            let target = File::open(directory.0.join("target.md")).unwrap();
            assert_current_user_only_dacl(&target);
            assert_eq!(fs::read(directory.0.join("target.md")).unwrap(), b"after");
        }

        fn assert_current_user_only_dacl(file: &File) {
            let mut dacl = ptr::null_mut();
            let mut descriptor = ptr::null_mut();
            let status = unsafe {
                GetSecurityInfo(
                    file.as_raw_handle(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &raw mut dacl,
                    ptr::null_mut(),
                    &raw mut descriptor,
                )
            };
            assert_eq!(status, ERROR_SUCCESS);

            let mut control = 0;
            let mut revision = 0;
            assert_ne!(
                unsafe {
                    GetSecurityDescriptorControl(descriptor, &raw mut control, &raw mut revision)
                },
                0
            );
            assert_ne!(control & SE_DACL_PROTECTED, 0);

            let mut information = ACL_SIZE_INFORMATION::default();
            assert_ne!(
                unsafe {
                    GetAclInformation(
                        dacl,
                        (&raw mut information).cast(),
                        mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                        AclSizeInformation,
                    )
                },
                0
            );
            assert_eq!(information.AceCount, 1);

            let mut ace = ptr::null_mut();
            assert_ne!(unsafe { GetAce(dacl, 0, &raw mut ace) }, 0);
            let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };

            let mut token = ptr::null_mut();
            assert_ne!(
                unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) },
                0
            );
            let token = unsafe { OwnedHandle::from_raw_handle(token) };
            let user = token_information(token.as_raw_handle(), TokenUser).unwrap();
            let user = unsafe { &*user.as_ptr().cast::<TOKEN_USER>() };
            assert_ne!(
                unsafe { EqualSid((&raw const ace.SidStart).cast_mut().cast(), user.User.Sid) },
                0
            );
            let _ = unsafe { LocalFree(descriptor) };
        }
    }
}

#[cfg(windows)]
pub use imp::{
    create_current_user_only_file, disable_std_handle_inheritance,
    replace_file_preserving_security, restrict_default_dacl_to_current_user, DefaultDaclGuard,
};
